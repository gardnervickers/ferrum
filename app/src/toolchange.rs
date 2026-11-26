//! Manual tool change with the power drawbar (milestone 4).
//!
//! Matches mymill_vcp/toolchange_dialog.py and the M6 remap
//! (subroutines/m6_toolchange_optional_touch_off.ngc):
//! - M6 raises iocontrol.0.tool-change; the overlay opens for the tool number.
//! - RESUME needs the spindle command off, the drawbar retracted and the
//!   operator's "tool seated" acknowledgement. It clears any stale touch-off
//!   request, then answers tool-changed.
//! - RESUME + TOUCH OFF also sets tool-touch-off-request-latch; the remap
//!   then runs tool_touch_off.ngc. Not for the probe (T99) or no tool.
//! - ABORT clears the touch-off request, retracts the drawbar if it is
//!   released, and aborts the program.
//! - Scratch tools (`[toolchange]scratch_tools`, T200–249 on mymill): warn
//!   that a touch off is likely required.
//!
//! The Pi decides through HAL (`HalIo`, pins on the drawbar module); the
//! simulator stands in with `SimIo`.

use crate::config::Config;
use crate::{Actions, Live, MainWindow};
use lcnc::hal::{Component, In, Out};
use machine::{Command, Handle, Status};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// What the overlay reads each tick. `None` = unknown (pin missing).
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    pub change: bool,
    pub number: i32,
    pub spindle_request: Option<bool>,
    pub drawbar_extended: Option<bool>,
    pub rearm_required: Option<bool>,
    pub nose_reference: Option<bool>,
    pub touch_off_latched: Option<bool>,
    pub manual_request: bool,
}

pub trait Io {
    /// A power drawbar and the touch-off latch are there (mymill). Without
    /// them the dialog only asks for the tool to be seated.
    fn drawbar(&self) -> bool {
        true
    }
    fn read(&self) -> Inputs;
    fn set_changed(&self, on: bool);
    fn set_manual_request(&self, on: bool);
    fn set_touch_off(&self, on: bool);
    fn set_touch_off_reset(&self, on: bool);
}

/// The M6 handshake (`ferrum.toolchange.*`) and the drawbar pins (`ferrum.drawbar.*`).
pub struct HalIo {
    change: In<bool>,
    number: In<i32>,
    changed: Out<bool>,
    spindle_request: In<bool>,
    extended: In<bool>,
    rearm_required: In<bool>,
    nose_reference: In<bool>,
    touch_off_latched: In<bool>,
    manual_request: Out<bool>,
    touch_off_set: Out<bool>,
    touch_off_reset: Out<bool>,
    manual: Cell<bool>,
}

impl HalIo {
    pub fn create(comp: &Component, extended: In<bool>, rearm_required: In<bool>) -> Result<Self, String> {
        Ok(Self {
            change: comp.pin("toolchange.tool-change")?,
            number: comp.pin("toolchange.tool-number")?,
            changed: comp.pin("toolchange.tool-changed")?,
            spindle_request: comp.pin("toolchange.spindle-on")?,
            extended,
            rearm_required,
            nose_reference: comp.pin("drawbar.nose-reference-valid")?,
            touch_off_latched: comp.pin("drawbar.touch-off-requested")?,
            manual_request: comp.pin("drawbar.manual-request")?,
            touch_off_set: comp.pin("drawbar.touch-off-set")?,
            touch_off_reset: comp.pin("drawbar.touch-off-reset")?,
            manual: Cell::new(false),
        })
    }
    pub fn extended(&self) -> bool {
        self.extended.get()
    }
    pub fn rearm_required(&self) -> bool {
        self.rearm_required.get()
    }
}

impl Io for HalIo {
    fn read(&self) -> Inputs {
        Inputs {
            change: self.change.get(),
            number: self.number.get(),
            spindle_request: Some(self.spindle_request.get()),
            drawbar_extended: Some(self.extended.get()),
            rearm_required: Some(self.rearm_required.get()),
            nose_reference: Some(self.nose_reference.get()),
            touch_off_latched: Some(self.touch_off_latched.get()),
            manual_request: self.manual.get(),
        }
    }
    fn set_changed(&self, on: bool) {
        self.changed.set(on);
    }
    fn set_manual_request(&self, on: bool) {
        self.manual.set(on);
        self.manual_request.set(on);
    }
    fn set_touch_off(&self, on: bool) {
        self.touch_off_set.set(on);
    }
    fn set_touch_off_reset(&self, on: bool) {
        self.touch_off_reset.set(on);
    }
}

/// The plain dialog (the `toolchange` module): iocontrol's tool change
/// request and number, the spindle command, and the answer.
pub struct DialogIo {
    change: In<bool>,
    number: In<i32>,
    changed: Out<bool>,
    spindle_on: In<bool>,
}

impl DialogIo {
    pub fn create(comp: &Component) -> Result<Self, String> {
        Ok(Self {
            change: comp.pin("toolchange.tool-change")?,
            number: comp.pin("toolchange.tool-number")?,
            changed: comp.pin("toolchange.tool-changed")?,
            spindle_on: comp.pin("toolchange.spindle-on")?,
        })
    }
}

impl Io for DialogIo {
    fn drawbar(&self) -> bool {
        false
    }
    fn read(&self) -> Inputs {
        Inputs { change: self.change.get(), number: self.number.get(), spindle_request: Some(self.spindle_on.get()), ..Inputs::default() }
    }
    fn set_changed(&self, on: bool) {
        self.changed.set(on);
    }
    fn set_manual_request(&self, _: bool) {}
    fn set_touch_off(&self, _: bool) {}
    fn set_touch_off_reset(&self, _: bool) {}
}

/// The simulator's stand-in: the M6 request comes from the simulated status,
/// the drawbar follows the manual request when the spindle is off.
pub struct SimIo {
    machine: Handle,
    status: RefCell<Option<Status>>,
    manual: Cell<bool>,
    latched: Cell<bool>,
    set: Cell<bool>,
}

impl SimIo {
    pub fn new(machine: Handle) -> Self {
        Self { machine, status: RefCell::new(None), manual: Cell::new(false), latched: Cell::new(false), set: Cell::new(false) }
    }
    pub fn status(&self, s: &Status) {
        *self.status.borrow_mut() = Some(s.clone());
    }
}

impl Io for SimIo {
    fn read(&self) -> Inputs {
        let st = self.status.borrow();
        let pending = st.as_ref().and_then(|s| s.sim_tool_change);
        let spindle = st.as_ref().is_some_and(|s| s.spindle_enabled);
        Inputs {
            change: pending.is_some(),
            number: pending.unwrap_or(0),
            spindle_request: Some(spindle),
            drawbar_extended: Some(self.manual.get() && !spindle),
            rearm_required: Some(false),
            nose_reference: Some(st.as_ref().is_some_and(|s| s.all_homed())),
            touch_off_latched: Some(self.latched.get()),
            manual_request: self.manual.get(),
        }
    }
    fn set_changed(&self, on: bool) {
        if on {
            if self.latched.replace(false) {
                self.machine.send(Command::Mdi("(sim: the remap would run tool_touch_off.ngc now)".into()));
            }
            self.machine.sim_tool_changed();
        }
    }
    fn set_manual_request(&self, on: bool) {
        self.manual.set(on);
    }
    fn set_touch_off(&self, on: bool) {
        if on && !self.set.get() {
            self.latched.set(true);
        }
        self.set.set(on);
    }
    fn set_touch_off_reset(&self, on: bool) {
        if on {
            self.latched.set(false);
        }
    }
}

pub struct ToolChange {
    io: Rc<dyn Io>,
    machine: Handle,
    probe_tool: i32,
    open: bool,
    /// Answered (resume or abort) and waiting for tool-change to drop.
    answered: bool,
    seated: bool,
    last: Inputs,
}

fn pulse(f: impl Fn(bool)) {
    f(true);
    std::thread::sleep(Duration::from_millis(50));
    f(false);
}

impl ToolChange {
    fn spindle_off(&self) -> bool {
        self.last.spindle_request == Some(false)
    }
    fn retracted(&self) -> bool {
        !self.io.drawbar() || self.last.drawbar_extended == Some(false)
    }
    fn resume_safe(&self) -> bool {
        self.spindle_off() && self.retracted() && self.seated
    }
    fn touch_off_allowed(&self) -> bool {
        let t = self.last.number;
        self.io.drawbar() && self.probe_tool > 0 && t > 0 && t != self.probe_tool
    }
    fn retract_allowed(&self) -> bool {
        self.spindle_off() || (self.last.spindle_request == Some(true) && self.last.rearm_required == Some(true))
    }

    /// Clear any touch-off request: the latch's set low, and a reset pulse.
    fn clear_touch_off(&self) -> bool {
        if !self.io.drawbar() {
            return true;
        }
        self.io.set_touch_off(false);
        pulse(|on| self.io.set_touch_off_reset(on));
        self.io.read().touch_off_latched == Some(false)
    }

    fn resume(&mut self, ui: &MainWindow, touch_off: bool) {
        self.last = self.io.read();
        let live = ui.global::<Live>();
        let fail = |why: &str| {
            live.set_tc_message(why.into());
        };
        if !self.resume_safe() {
            return fail("Not safe to resume: spindle off, drawbar retracted and tool seated are all required.");
        }
        // A drill can be changed in its chuck without cycling the drawbar;
        // still require a cleared request before M6 resumes.
        self.io.set_manual_request(false);
        if !self.clear_touch_off() {
            return fail("Could not clear the touch-off request. Check HAL.");
        }
        if touch_off {
            if !self.touch_off_allowed() {
                return fail("Touch off is not available for this tool.");
            }
            pulse(|on| self.io.set_touch_off(on));
            if self.io.read().touch_off_latched != Some(true) {
                self.clear_touch_off();
                return fail("The touch-off request did not latch. Check HAL.");
            }
        }
        self.last = self.io.read();
        if !self.resume_safe() {
            self.clear_touch_off();
            return fail("NOT SAFE TO RESUME.");
        }
        self.io.set_changed(true);
        self.open = false;
        self.answered = true;
        live.set_tc_open(false);
    }

    fn abort(&mut self, ui: &MainWindow) {
        self.clear_touch_off();
        if self.retract_allowed() {
            self.io.set_manual_request(false);
        }
        self.machine.send(Command::Abort);
        self.open = false;
        self.answered = true;
        ui.global::<Live>().set_tc_open(false);
    }

    fn tick(&mut self, ui: &MainWindow) {
        let i = self.io.read();
        // Handshake: tool-changed follows tool-change back down.
        if !i.change {
            self.io.set_changed(false);
            self.answered = false;
        }
        if i.change && !self.open && !self.answered {
            self.open = true;
            self.seated = false;
            self.clear_touch_off();
        } else if !i.change && self.open {
            self.open = false;
        }
        self.last = i;
        self.show(ui);
    }

    fn show(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        live.set_tc_open(self.open);
        if !self.open {
            return;
        }
        let i = &self.last;
        let t = i.number;
        live.set_tc_tool(if t > 0 { format!("T{t}") } else { "NO TOOL (T0)".into() }.into());
        live.set_tc_drawbar(
            match i.drawbar_extended {
                Some(true) => "EXTENDED",
                Some(false) => "RETRACTED",
                None => "UNKNOWN",
            }
            .into(),
        );
        live.set_tc_seated(self.seated);
        live.set_tc_has_drawbar(self.io.drawbar());
        let msg = if i.spindle_request == Some(true) {
            "Spindle command active. Acknowledge only when the tool change is safe."
        } else if !self.io.drawbar() && !self.seated {
            "Load the tool, verify it is secure, then acknowledge below."
        } else if !self.io.drawbar() {
            "Tool acknowledged. Resume when ready."
        } else if i.drawbar_extended == Some(true) {
            "Drawbar extended. Retract if needed, then acknowledge when the tool is secure."
        } else if i.drawbar_extended.is_none() {
            "Drawbar status unknown. Acknowledge only after the tool is secure."
        } else if !self.seated {
            "Load the tool, verify it is seated, then acknowledge below."
        } else {
            "Tool seated acknowledged. Resume or touch off when ready."
        };
        live.set_tc_message(msg.into());
        live.set_tc_can_extend(i.spindle_request == Some(false));
        live.set_tc_can_retract((i.manual_request || i.drawbar_extended == Some(true)) && self.retract_allowed());
        live.set_tc_can_resume(self.resume_safe());
        live.set_tc_can_touch_off(self.resume_safe() && self.touch_off_allowed());
        live.set_tc_touch_off_tip(
            if t == self.probe_tool { "The probe is calibrated on a reference surface, not here" } else { "Measure the tool on the fixed toolsetter" }.into(),
        );
        live.set_tc_scratch(if crate::machine_cfg::get().is_scratch_tool(t) { format!("Scratch tool T{t}: a tool touch off is likely required before cutting.") } else { String::new() }.into());
        let (text, kind) = match i.nose_reference {
            Some(true) => ("NOSE REFERENCE UPDATED", "good"),
            Some(false) => ("NOSE REFERENCE NOT UPDATED", "warn"),
            None => ("NOSE REFERENCE UNKNOWN", ""),
        };
        live.set_tc_nose(text.into());
        live.set_tc_nose_kind(kind.into());
    }
}

/// Wire the overlay. Returns the timer that polls the I/O (keep it alive).
pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, io: Rc<dyn Io>) -> slint::Timer {
    let probe_tool = crate::machine_cfg::get().probe_tool(cfg);
    let tc = Rc::new(RefCell::new(ToolChange { io, machine: machine.clone(), probe_tool, open: false, answered: false, seated: false, last: Inputs::default() }));
    let actions = ui.global::<Actions>();
    let (t, weak) = (tc.clone(), ui.as_weak());
    actions.on_tc_seated_toggle(move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut tc = t.borrow_mut();
        tc.seated = !tc.seated;
        tc.show(&ui);
    });
    let (t, weak) = (tc.clone(), ui.as_weak());
    actions.on_tc_drawbar(move |extend| {
        let Some(ui) = weak.upgrade() else { return };
        let tc = t.borrow();
        let ok = if extend { tc.spindle_off() } else { tc.retract_allowed() };
        if ok {
            tc.io.set_manual_request(extend);
        }
        drop(tc);
        t.borrow_mut().tick(&ui);
    });
    let (t, weak) = (tc.clone(), ui.as_weak());
    actions.on_tc_resume(move |touch_off| {
        if let Some(ui) = weak.upgrade() {
            t.borrow_mut().resume(&ui, touch_off);
        }
    });
    let (t, weak) = (tc.clone(), ui.as_weak());
    actions.on_tc_abort(move || {
        if let Some(ui) = weak.upgrade() {
            t.borrow_mut().abort(&ui);
        }
    });
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        if let Some(ui) = weak.upgrade() {
            tc.borrow_mut().tick(&ui);
        }
    });
    timer
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(RefCell<Inputs>, Cell<bool>);
    impl Io for Fake {
        fn read(&self) -> Inputs {
            self.0.borrow().clone()
        }
        fn set_changed(&self, on: bool) {
            self.1.set(on);
        }
        fn set_manual_request(&self, on: bool) {
            self.0.borrow_mut().manual_request = on;
        }
        fn set_touch_off(&self, on: bool) {
            if on {
                self.0.borrow_mut().touch_off_latched = Some(true);
            }
        }
        fn set_touch_off_reset(&self, on: bool) {
            if on {
                self.0.borrow_mut().touch_off_latched = Some(false);
            }
        }
    }

    fn tc(number: i32) -> ToolChange {
        let io = Rc::new(Fake(
            RefCell::new(Inputs { change: true, number, spindle_request: Some(false), drawbar_extended: Some(false), touch_off_latched: Some(false), ..Default::default() }),
            Cell::new(false),
        ));
        let mut t = ToolChange { io: io.clone(), machine: machine::Handle::detached(), probe_tool: 99, open: true, answered: false, seated: false, last: Inputs::default() };
        t.last = io.read();
        t
    }

    #[test]
    fn resume_needs_seated_retracted_and_spindle_off() {
        let mut t = tc(5);
        assert!(!t.resume_safe());
        t.seated = true;
        assert!(t.resume_safe());
        t.last.drawbar_extended = Some(true);
        assert!(!t.resume_safe());
        t.last.drawbar_extended = None;
        assert!(!t.resume_safe());
        t.last.drawbar_extended = Some(false);
        t.last.spindle_request = Some(true);
        assert!(!t.resume_safe());
    }

    #[test]
    fn touch_off_not_for_probe_or_no_tool() {
        assert!(tc(5).touch_off_allowed());
        assert!(!tc(99).touch_off_allowed());
        assert!(!tc(0).touch_off_allowed());
    }
}
