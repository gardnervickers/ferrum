//! A frozen or crashed screen must never leave the machine live.
//!
//! - Heartbeat: `ferrum.watchdog.heartbeat` flips once per frame that is
//!   actually drawn (femtovg: after rendering, before the buffer swap;
//!   software: when winit asks the window to redraw). A timer only asks for
//!   frames, so a live event loop with a dead window does not beat. A
//!   realtime `oneshot` in HAL (ferrum/hal/watchdog.hal, or the built-in
//!   wiring in modules.rs) trips E-stop when the beats stop for
//!   [`TIMEOUT`]: a frozen window, a hung or killed process, a crash.
//! - Keep-alive: at idle nothing redraws, so a timer asks for a frame every
//!   [`KEEPALIVE`].
//! - Stale input: a tap or key press that arrives when no frame has been
//!   drawn for [`STALE`] is dropped. What is under the finger is not what
//!   the operator sees.

use crate::MainWindow;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// How often a frame is asked for when nothing else redraws.
pub const KEEPALIVE: Duration = Duration::from_millis(250);
/// Input older than this past the last drawn frame is dropped.
pub const STALE: Duration = Duration::from_millis(600);
/// The HAL watchdog's timeout (`oneshot.width`), in seconds.
pub const TIMEOUT: f64 = 1.0;

#[derive(Default)]
struct State {
    pin: Option<lcnc::hal::Out<bool>>,
    beat: bool,
    last: Option<Instant>,
    frames: u64,
    /// Longest time between two drawn frames since `reset_max_gap`.
    max_gap: Duration,
    dropped: u64,
    /// The rendering notifier marks frames (femtovg); the winit
    /// RedrawRequested hook must not count them twice.
    notifier: bool,
    /// The watchdog has seen the screen live (set by modules.rs once HAL
    /// says ok): gaps from here on are freezes, not the start-up.
    armed: bool,
    /// The last gap long enough for HAL to have E-stopped the machine.
    froze: Option<Duration>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// The heartbeat pin, once HAL is up (modules.rs, Watchdog).
pub fn set_pin(pin: lcnc::hal::Out<bool>) {
    STATE.with(|s| s.borrow_mut().pin = Some(pin));
}

/// The renderer's notifier marks frames from now on.
pub fn notifier_marks_frames() {
    STATE.with(|s| s.borrow_mut().notifier = true);
}

/// A frame was drawn.
pub fn frame_drawn() {
    mark(Instant::now());
}

fn mark(now: Instant) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(last) = s.last {
            let gap = now.saturating_duration_since(last);
            if gap > s.max_gap {
                s.max_gap = gap;
            }
            if s.armed && gap.as_secs_f64() >= TIMEOUT * 0.9 {
                s.froze = Some(gap);
                eprintln!("ferrum: the screen did not draw for {} ms: the HAL screen watchdog E-stops the machine", gap.as_millis());
            }
        }
        s.last = Some(now);
        s.frames += 1;
        s.beat = !s.beat;
        let b = s.beat;
        if let Some(p) = &s.pin {
            p.set(b);
        }
    });
}

/// The watchdog saw the screen live: later long gaps count as freezes.
pub fn arm() {
    STATE.with(|s| s.borrow_mut().armed = true);
}

/// The last freeze long enough to E-stop, until `clear_froze`.
pub fn froze() -> Option<Duration> {
    STATE.with(|s| s.borrow().froze)
}

pub fn clear_froze() {
    STATE.with(|s| s.borrow_mut().froze = None);
}

/// True when no frame has been drawn for [`STALE`] (or none at all).
pub fn stale(now: Instant) -> bool {
    STATE.with(|s| s.borrow().last.is_none_or(|t| now.saturating_duration_since(t) > STALE))
}

/// (frames drawn, longest gap between frames in ms, input events dropped).
pub fn stats() -> (u64, u64, u64) {
    STATE.with(|s| {
        let s = s.borrow();
        (s.frames, s.max_gap.as_millis() as u64, s.dropped)
    })
}

/// Forget the longest gap so far (the harness does this once booted).
pub fn reset_max_gap() {
    STATE.with(|s| s.borrow_mut().max_gap = Duration::ZERO);
}

/// Input that acts on what is on screen.
#[cfg(not(target_os = "wasi"))]
fn is_input(ev: &slint::winit_030::winit::event::WindowEvent) -> bool {
    use slint::winit_030::winit::event::WindowEvent as W;
    matches!(ev, W::MouseInput { .. } | W::Touch(_) | W::MouseWheel { .. } | W::KeyboardInput { .. } | W::PinchGesture { .. } | W::PanGesture { .. })
}

/// Install the window's one winit event filter: frame marking for the
/// software renderer, the stale-input guard (`guard_input`), then `extra`
/// (the desktop window fit).
#[cfg(not(target_os = "wasi"))]
pub fn install_filter(
    ui: &MainWindow,
    guard_input: bool,
    mut extra: Option<Box<dyn FnMut(&slint::Window, &slint::winit_030::winit::event::WindowEvent) -> slint::winit_030::EventResult>>,
) {
    use slint::winit_030::{winit::event::WindowEvent, EventResult, WinitWindowAccessor};
    ui.window().on_winit_window_event(move |w, ev| {
        if matches!(ev, WindowEvent::RedrawRequested) && !STATE.with(|s| s.borrow().notifier) {
            frame_drawn();
        }
        if guard_input && is_input(ev) && stale(Instant::now()) {
            let n = STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.dropped += 1;
                s.dropped
            });
            if n == 1 || n % 50 == 0 {
                eprintln!("ferrum: input ignored: the screen has not drawn for over {} ms ({n} so far)", STALE.as_millis());
            }
            return EventResult::PreventDefault;
        }
        match extra.as_mut() {
            Some(f) => f(w, ev),
            None => EventResult::Propagate,
        }
    });
}

/// Ask for a frame every [`KEEPALIVE`], so the heartbeat runs at idle.
pub fn start_keepalive(ui: &MainWindow) -> slint::Timer {
    let t = slint::Timer::default();
    let weak = ui.as_weak();
    t.start(slint::TimerMode::Repeated, KEEPALIVE, move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().request_redraw();
        }
    });
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_make_the_screen_fresh_and_track_the_gap() {
        let t0 = Instant::now();
        assert!(stale(t0), "no frame yet is stale");
        mark(t0);
        assert!(!stale(t0 + Duration::from_millis(100)));
        assert!(stale(t0 + STALE + Duration::from_millis(1)));
        mark(t0 + Duration::from_millis(900));
        let (frames, gap, _) = stats();
        assert_eq!(frames, 2);
        assert_eq!(gap, 900);
        reset_max_gap();
        assert_eq!(stats().1, 0);
    }
}
