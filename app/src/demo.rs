//! The browser demo: Ferrum and its simulated machine built for WASI and run
//! in a Web Worker (web/static/demo.js), with no LinuxCNC, HAL or network.
//!
//! The page's screen is the ordinary browser build (web/), which mirrors a
//! Ferrum over the remote link (remote.rs). Here that link is two imported
//! functions out and an exported one in, instead of a websocket. There is no
//! window: the page draws. The host calls [`ferrum_demo_tick`] every frame,
//! which stands in for Slint's event loop and the machine thread.
//!
//! Only the simulator exists in this build (`connect_machine` is never asked
//! for NML: WASI has no LinuxCNC), so it can never move a real machine.

use crate::checks::Hal;
use crate::config::Config;
use crate::modules::Module;
use crate::{Args, MainWindow};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Queue = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

struct DemoPlatform {
    window: Rc<MinimalSoftwareWindow>,
    queue: Queue,
    t0: Instant,
}

struct Proxy(Queue);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(())
    }
    fn invoke_from_event_loop(&self, event: Box<dyn FnOnce() + Send>) -> Result<(), slint::EventLoopError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

impl Platform for DemoPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.t0.elapsed()
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
}

struct Demo {
    ui: MainWindow,
    queue: Queue,
    window: Rc<MinimalSoftwareWindow>,
    /// Slint makes a screen's parts (and their timers and size callbacks)
    /// only when it draws them, so this window is drawn now and then, to
    /// nothing anyone sees.
    frame: Vec<Rgb565Pixel>,
    drawn_at: Option<Instant>,
}

/// How often the unseen window is drawn.
const DRAW_EVERY: Duration = Duration::from_millis(200);

thread_local! {
    static DEMO: RefCell<Option<Demo>> = const { RefCell::new(None) };
}

pub fn ui_weak() -> Option<slint::Weak<MainWindow>> {
    DEMO.with(|d| d.borrow().as_ref().map(|d| d.ui.as_weak()))
}

pub fn start(args: Args, cfg: Config, modules: Vec<Box<dyn Module>>, notes: Vec<String>, hal: Box<dyn Hal>) {
    i_slint_core::OPERATING_SYSTEM_OVERRIDE.with(|os| os.set(Some(i_slint_core::OperatingSystemType::Linux)));
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    window.set_size(PhysicalSize::new(W, H));
    let queue: Queue = Arc::default();
    slint::platform::set_platform(Box::new(DemoPlatform { window: window.clone(), queue: queue.clone(), t0: Instant::now() })).expect("set demo platform");
    let ui = MainWindow::new().expect("demo window");
    crate::boot::init_brand(&ui);
    let _ = ui.show();
    slint::spawn_local(crate::run(ui.clone_strong(), args, cfg, modules, notes, hal)).expect("start demo");
    DEMO.with(|d| *d.borrow_mut() = Some(Demo { ui, queue, window, frame: vec![Rgb565Pixel::default(); (W * H) as usize], drawn_at: None }));
}

/// The touchscreen's size, which the page's screen mirrors.
const W: u32 = 1920;
const H: u32 = 1080;

/// One turn of the event loop: queued calls, the machine, timers.
#[no_mangle]
pub extern "C" fn ferrum_demo_tick() {
    let Some(queue) = DEMO.with(|d| d.borrow().as_ref().map(|d| d.queue.clone())) else { return };
    let drain = || {
        // Calls may queue more calls; bounded so a loop of them still yields.
        for _ in 0..16 {
            let jobs = std::mem::take(&mut *queue.lock().unwrap());
            if jobs.is_empty() {
                break;
            }
            for j in jobs {
                j();
            }
        }
    };
    drain();
    machine::pump();
    drain();
    slint::platform::update_timers_and_animations();
    drain();
    DEMO.with(|d| {
        let mut d = d.borrow_mut();
        let Some(d) = d.as_mut() else { return };
        if d.drawn_at.is_some_and(|t| t.elapsed() < DRAW_EVERY) {
            return;
        }
        d.drawn_at = Some(Instant::now());
        let Demo { window, frame, .. } = d;
        window.draw_if_needed(|r| {
            r.render(frame, W as usize);
        });
    });
}

/// Room for a message from the page; it writes there, then calls
/// [`ferrum_demo_recv`], which frees it.
#[no_mangle]
pub extern "C" fn ferrum_demo_alloc(len: usize) -> *mut u8 {
    Box::into_raw(vec![0u8; len].into_boxed_slice()) as *mut u8
}

/// A message from the page's screen (the websocket protocol's text frames).
#[no_mangle]
pub extern "C" fn ferrum_demo_recv(ptr: *mut u8, len: usize) {
    let bytes = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) };
    if let Ok(text) = String::from_utf8(bytes.into_vec()) {
        crate::remote::demo_receive(&text);
    }
}
