//! A userspace HAL component, with pins typed by value and direction.
//!
//! `liblinuxcnchal.so.0` is loaded with `dlopen` when the first component
//! is made, so building needs no LinuxCNC headers or libraries. The HAL ABI
//! used here (component and pin calls from LinuxCNC 2.9's `hal.h`) is
//! small and has been stable for years.
//!
//! ```no_run
//! use lcnc::hal::{Component, In, Out};
//!
//! let comp = Component::new("mygui")?;
//! let request: In<bool> = comp.pin("tool-change")?;
//! let done: Out<bool> = comp.pin("tool-changed")?;
//! let tool: In<i32> = comp.pin("tool-number")?;
//! comp.ready()?;
//! if request.get() {
//!     println!("change to T{}", tool.get());
//!     done.set(true);
//! }
//! # Ok::<(), lcnc::Error>(())
//! ```
//!
//! An input pin has no `set`, so writing to one doesn't compile:
//!
//! ```compile_fail
//! # use lcnc::hal::{Component, In};
//! # let comp = Component::new("x").unwrap();
//! let p: In<bool> = comp.pin("a").unwrap();
//! p.set(true);
//! ```
//!
//! Pins keep their component alive; HAL's `hal_exit` runs when the
//! component and all its pins are dropped.

use crate::error::{Error, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_long, c_void, CString};
use std::marker::PhantomData;
use std::ptr;
use std::sync::{Arc, OnceLock};

const HAL_LIB: &str = "liblinuxcnchal.so.0";

// hal_pin_dir_t from hal.h
const HAL_IN: c_int = 16;
const HAL_OUT: c_int = 32;
const HAL_IO: c_int = 48;

type PinNew<T> = unsafe extern "C" fn(*const c_char, c_int, *mut *mut T, c_int) -> c_int;

/// The HAL entry points, resolved once per process.
#[doc(hidden)]
pub struct Api {
    _lib: Library,
    init: unsafe extern "C" fn(*const c_char) -> c_int,
    ready: unsafe extern "C" fn(c_int) -> c_int,
    exit: unsafe extern "C" fn(c_int) -> c_int,
    malloc: unsafe extern "C" fn(c_long) -> *mut c_void,
    bit_new: PinNew<u8>,
    s32_new: PinNew<i32>,
    u32_new: PinNew<u32>,
    float_new: PinNew<f64>,
}

fn api() -> Result<&'static Api> {
    static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let lib = Library::new(HAL_LIB).map_err(|e| format!("dlopen {HAL_LIB}: {e}"))?;
        macro_rules! sym {
            ($name:literal) => {
                *lib.get($name).map_err(|e| format!("{}: {e}", String::from_utf8_lossy($name)))?
            };
        }
        Ok(Api {
            init: sym!(b"hal_init\0"),
            ready: sym!(b"hal_ready\0"),
            exit: sym!(b"hal_exit\0"),
            malloc: sym!(b"hal_malloc\0"),
            bit_new: sym!(b"hal_pin_bit_new\0"),
            s32_new: sym!(b"hal_pin_s32_new\0"),
            u32_new: sym!(b"hal_pin_u32_new\0"),
            float_new: sym!(b"hal_pin_float_new\0"),
            _lib: lib,
        })
    })
    .as_ref()
    .map_err(|e| Error::Hal(e.clone()))
}

mod sealed {
    pub trait Sealed {}
}

/// A HAL pin type: `bool` (bit), `i32` (s32), `u32` (u32) or `f64` (float).
pub trait PinType: sealed::Sealed + Copy {
    #[doc(hidden)]
    type Raw: Copy;
    #[doc(hidden)]
    fn new_fn(api: &Api) -> PinNew<Self::Raw>;
    #[doc(hidden)]
    fn from_raw(r: Self::Raw) -> Self;
    #[doc(hidden)]
    fn to_raw(self) -> Self::Raw;
}

macro_rules! pin_type {
    ($t:ty, $raw:ty, $f:ident, $from:expr, $to:expr) => {
        impl sealed::Sealed for $t {}
        impl PinType for $t {
            type Raw = $raw;
            fn new_fn(api: &Api) -> PinNew<$raw> {
                api.$f
            }
            fn from_raw(r: $raw) -> Self {
                ($from)(r)
            }
            fn to_raw(self) -> $raw {
                ($to)(self)
            }
        }
    };
}

pin_type!(bool, u8, bit_new, |r: u8| r != 0, |v: bool| v as u8);
pin_type!(i32, i32, s32_new, |r| r, |v| v);
pin_type!(u32, u32, u32_new, |r| r, |v| v);
pin_type!(f64, f64, float_new, |r| r, |v| v);

/// A pin direction marker: [`Input`], [`Output`] or [`InOut`].
pub trait Direction: sealed::Sealed {
    #[doc(hidden)]
    const RAW: c_int;
}

/// HAL_IN: read only.
pub enum Input {}
/// HAL_OUT: written by this component.
pub enum Output {}
/// HAL_IO: read and written.
pub enum InOut {}

impl sealed::Sealed for Input {}
impl sealed::Sealed for Output {}
impl sealed::Sealed for InOut {}
impl Direction for Input {
    const RAW: c_int = HAL_IN;
}
impl Direction for Output {
    const RAW: c_int = HAL_OUT;
}
impl Direction for InOut {
    const RAW: c_int = HAL_IO;
}

/// Directions a component may write.
pub trait Writable: Direction {}
impl Writable for Output {}
impl Writable for InOut {}

struct Comp {
    id: c_int,
    prefix: String,
}

impl Drop for Comp {
    fn drop(&mut self) {
        if let Ok(api) = api() {
            unsafe { (api.exit)(self.id) };
        }
    }
}

/// A HAL pin of type `T` and direction `D`. The value lives in HAL shared
/// memory, shared with realtime threads, so every access is a volatile
/// read or write of the pointer HAL gave the component.
pub struct Pin<T: PinType, D: Direction> {
    slot: *mut *mut T::Raw,
    _comp: Arc<Comp>,
    _d: PhantomData<D>,
}

// The pin is a pointer into HAL shared memory; moving it to another
// thread is fine. It is not Sync: HAL pins have no locking.
unsafe impl<T: PinType, D: Direction> Send for Pin<T, D> {}

/// An input pin.
pub type In<T> = Pin<T, Input>;
/// An output pin.
pub type Out<T> = Pin<T, Output>;
/// A bidirectional pin.
pub type Io<T> = Pin<T, InOut>;

impl<T: PinType, D: Direction> Pin<T, D> {
    pub fn get(&self) -> T {
        T::from_raw(unsafe { ptr::read_volatile(*self.slot) })
    }
}

impl<T: PinType, D: Writable> Pin<T, D> {
    pub fn set(&self, v: T) {
        unsafe { ptr::write_volatile(*self.slot, v.to_raw()) }
    }
}

/// A userspace HAL component. Make its pins, then call [`ready`](Self::ready).
pub struct Component {
    comp: Arc<Comp>,
}

impl Component {
    /// `hal_init(name)`. Pin names are prefixed with `name.`.
    pub fn new(name: &str) -> Result<Self> {
        let api = api()?;
        let cname = CString::new(name).map_err(|_| Error::Nul)?;
        let id = unsafe { (api.init)(cname.as_ptr()) };
        if id < 0 {
            return Err(Error::Hal(format!("hal_init({name}) failed: {id}")));
        }
        Ok(Self { comp: Arc::new(Comp { id, prefix: name.to_string() }) })
    }

    /// The component's name.
    pub fn name(&self) -> &str {
        &self.comp.prefix
    }

    /// Make pin `<component>.<name>`. The type and direction come from the
    /// binding: `let p: Out<f64> = comp.pin("speed")?;`.
    pub fn pin<T: PinType, D: Direction>(&self, name: &str) -> Result<Pin<T, D>> {
        let api = api()?;
        let full = format!("{}.{}", self.comp.prefix, name);
        let cname = CString::new(full.as_str()).map_err(|_| Error::Nul)?;
        // HAL wants the pointer to the pin's storage itself in HAL memory.
        let slot = unsafe { (api.malloc)(std::mem::size_of::<*mut T::Raw>() as c_long) } as *mut *mut T::Raw;
        if slot.is_null() {
            return Err(Error::Hal("hal_malloc failed".into()));
        }
        let rc = unsafe { (T::new_fn(api))(cname.as_ptr(), D::RAW, slot, self.comp.id) };
        if rc != 0 {
            return Err(Error::Hal(format!("cannot make pin {full}: {rc}")));
        }
        Ok(Pin { slot, _comp: self.comp.clone(), _d: PhantomData })
    }

    /// `hal_ready`: the component is complete and can be netted.
    pub fn ready(&self) -> Result<()> {
        let rc = unsafe { (api()?.ready)(self.comp.id) };
        if rc != 0 {
            return Err(Error::Hal(format!("hal_ready failed: {rc}")));
        }
        Ok(())
    }
}
