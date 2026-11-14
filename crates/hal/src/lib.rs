//! Minimal userspace HAL component, loaded from `liblinuxcnchal.so.0` at run time.
//!
//! Loading the library with dlopen keeps the cross build free of a Pi sysroot:
//! the binary links only against glibc, and the HAL ABI used here (LinuxCNC 2.9
//! `hal.h`) is small and stable.

use libloading::{Library, Symbol};
use std::ffi::{c_char, c_int, c_long, c_void, CString};
use std::ptr;

const HAL_LIB: &str = "liblinuxcnchal.so.0";

// hal_pin_dir_t from hal.h
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub enum Dir {
    In = 16,
    Out = 32,
    Io = 48,
}

type HalInit = unsafe extern "C" fn(*const c_char) -> c_int;
type HalReady = unsafe extern "C" fn(c_int) -> c_int;
type HalExit = unsafe extern "C" fn(c_int) -> c_int;
type HalMalloc = unsafe extern "C" fn(c_long) -> *mut c_void;
// int hal_pin_bit_new(const char *name, hal_pin_dir_t dir, hal_bit_t **data_ptr_addr, int comp_id)
type HalPinBitNew = unsafe extern "C" fn(*const c_char, c_int, *mut *mut u8, c_int) -> c_int;
type HalPinS32New = unsafe extern "C" fn(*const c_char, c_int, *mut *mut i32, c_int) -> c_int;
type HalPinFloatNew = unsafe extern "C" fn(*const c_char, c_int, *mut *mut f64, c_int) -> c_int;

/// A HAL `bit` pin. The pointed-to storage lives in HAL shared memory and is
/// shared with the realtime threads, so every access is volatile.
pub struct BitPin(*mut *mut u8);
pub struct S32Pin(*mut *mut i32);
/// A HAL `float` pin (a double).
pub struct FloatPin(*mut *mut f64);

impl FloatPin {
    pub fn get(&self) -> f64 {
        unsafe { ptr::read_volatile(*self.0) }
    }
}

impl BitPin {
    pub fn get(&self) -> bool {
        unsafe { ptr::read_volatile(*self.0) != 0 }
    }
    pub fn set(&self, v: bool) {
        unsafe { ptr::write_volatile(*self.0, v as u8) }
    }
}

impl S32Pin {
    #[allow(dead_code)]
    pub fn get(&self) -> i32 {
        unsafe { ptr::read_volatile(*self.0) }
    }
    pub fn set(&self, v: i32) {
        unsafe { ptr::write_volatile(*self.0, v) }
    }
}

pub struct Component {
    lib: Library,
    comp_id: c_int,
    prefix: String,
}

impl Component {
    pub fn new(name: &str) -> Result<Self, String> {
        let lib = unsafe { Library::new(HAL_LIB) }.map_err(|e| format!("dlopen {HAL_LIB}: {e}"))?;
        let cname = CString::new(name).unwrap();
        let comp_id = unsafe {
            let init: Symbol<HalInit> = lib.get(b"hal_init\0").map_err(|e| e.to_string())?;
            init(cname.as_ptr())
        };
        if comp_id < 0 {
            return Err(format!("hal_init({name}) failed: {comp_id}"));
        }
        Ok(Self { lib, comp_id, prefix: name.to_string() })
    }

    fn slot<T>(&self) -> Result<*mut *mut T, String> {
        let p = unsafe {
            let malloc: Symbol<HalMalloc> = self.lib.get(b"hal_malloc\0").map_err(|e| e.to_string())?;
            malloc(std::mem::size_of::<*mut T>() as c_long)
        };
        if p.is_null() {
            return Err("hal_malloc failed".into());
        }
        Ok(p as *mut *mut T)
    }

    fn full_name(&self, pin: &str) -> CString {
        CString::new(format!("{}.{}", self.prefix, pin)).unwrap()
    }

    pub fn bit(&self, pin: &str, dir: Dir) -> Result<BitPin, String> {
        let slot = self.slot::<u8>()?;
        let name = self.full_name(pin);
        let rc = unsafe {
            let f: Symbol<HalPinBitNew> = self.lib.get(b"hal_pin_bit_new\0").map_err(|e| e.to_string())?;
            f(name.as_ptr(), dir as c_int, slot, self.comp_id)
        };
        if rc != 0 {
            return Err(format!("hal_pin_bit_new({name:?}) failed: {rc}"));
        }
        Ok(BitPin(slot))
    }

    pub fn s32(&self, pin: &str, dir: Dir) -> Result<S32Pin, String> {
        let slot = self.slot::<i32>()?;
        let name = self.full_name(pin);
        let rc = unsafe {
            let f: Symbol<HalPinS32New> = self.lib.get(b"hal_pin_s32_new\0").map_err(|e| e.to_string())?;
            f(name.as_ptr(), dir as c_int, slot, self.comp_id)
        };
        if rc != 0 {
            return Err(format!("hal_pin_s32_new({name:?}) failed: {rc}"));
        }
        Ok(S32Pin(slot))
    }

    pub fn float(&self, pin: &str, dir: Dir) -> Result<FloatPin, String> {
        let slot = self.slot::<f64>()?;
        let name = self.full_name(pin);
        let rc = unsafe {
            let f: Symbol<HalPinFloatNew> = self.lib.get(b"hal_pin_float_new\0").map_err(|e| e.to_string())?;
            f(name.as_ptr(), dir as c_int, slot, self.comp_id)
        };
        if rc != 0 {
            return Err(format!("hal_pin_float_new({name:?}) failed: {rc}"));
        }
        Ok(FloatPin(slot))
    }

    pub fn ready(&self) -> Result<(), String> {
        let rc = unsafe {
            let f: Symbol<HalReady> = self.lib.get(b"hal_ready\0").map_err(|e| e.to_string())?;
            f(self.comp_id)
        };
        if rc != 0 {
            return Err(format!("hal_ready failed: {rc}"));
        }
        Ok(())
    }
}

impl Drop for Component {
    fn drop(&mut self) {
        unsafe {
            if let Ok(f) = self.lib.get::<HalExit>(b"hal_exit\0") {
                f(self.comp_id);
            }
        }
    }
}
