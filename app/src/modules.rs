//! Feature modules. Each machine capability or screen is one module; the
//! `[FERRUM]MODULES` list in ferrum.ini decides which exist. A module that
//! is not listed is never constructed: no nav entry, no HAL pins, no postgui
//! nets (`ferrum/hal/<id>.hal`), no status pills.

use crate::config::Config;
use lcnc::hal::{Component, In, Out};
use machine::Status;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub struct Pill {
    pub text: String,
    /// "" | "good" | "warn" | "bad" | "info"
    pub kind: &'static str,
    /// Shown in the status list: what a row that is not OK means.
    pub hint: &'static str,
}

impl Pill {
    pub fn new(text: impl Into<String>, kind: &'static str) -> Self {
        Self { text: text.into(), kind, hint: "" }
    }
    pub fn hint(mut self, hint: &'static str) -> Self {
        self.hint = hint;
        self
    }
}

pub struct NavEntry {
    pub id: &'static str,
    pub label: &'static str,
    /// SVG path data in a 24x24 box, stroked.
    pub icon: &'static str,
}

pub trait Module {
    fn id(&self) -> &'static str;

    /// The screen this module adds to the nav bar, if any.
    fn nav(&self) -> Option<NavEntry> {
        None
    }

    /// Create the HAL pins this module owns, before hal_ready. Pins are named
    /// as crate::halpins lists them.
    fn create_pins(&mut self, _comp: &Component) -> Result<(), String> {
        Ok(())
    }

    /// Before this module's pins are connected (crate::halpins), with its
    /// own HAL file not run: take pins away from what drove them.
    fn before_links(&mut self) {}

    /// A pin of this module's `[hal]` name was connected (or the module's
    /// old HAL file ran).
    fn linked(&mut self, _name: &str) {}

    /// Hardware buttons, knobs and alarms since the last call (every 50 ms).
    fn events(&self, _status: Option<&Status>) -> Vec<Event> {
        Vec::new()
    }

    /// What the screen shows, for the stack light and an MPG.
    fn outputs(&self, _o: &Outputs) {}

    /// A key switch says SETUP may not change settings (None: no switch).
    fn locked(&self) -> Option<bool> {
        None
    }

    /// Spindle feedback from HAL: (actual rpm, at speed), each None when
    /// the machine does not have it.
    fn spindle_feedback(&self) -> Option<(Option<f64>, Option<bool>)> {
        None
    }

    /// Status bar pills, refreshed several times a second.
    fn pills(&self, _status: Option<&Status>, _out: &mut Vec<Pill>) {}

    /// Why GUI-started motion should not begin now, if this module knows.
    /// A preflight for clear messages only; HAL holds motion regardless.
    fn motion_block(&self) -> Option<String> {
        None
    }

    /// Why the spindle should not start now (powerfeed.py's
    /// _spindle_start_block_reason). A clear message only; HAL enforces it.
    fn spindle_block(&self) -> Option<String> {
        None
    }

    /// Flags other screens check, e.g. "rotary" or "toolsetter".
    #[allow(dead_code)] // read by the screens from milestone 2 on
    fn capabilities(&self) -> &'static [&'static str] {
        &[]
    }

    /// The manual tool change I/O, from the module that owns the drawbar.
    fn toolchange(&self) -> Option<Rc<dyn crate::toolchange::Io>> {
        None
    }

    /// The tool setter and its air blast, from the probe module.
    fn setter(&self) -> Option<Rc<SetterHal>> {
        None
    }

    /// Cutting load now, 0 (idle) to 1 (full), from the module that has a
    /// load signal. The backplot colours the tool's trail by it.
    fn load(&self, _status: Option<&Status>) -> Option<f64> {
        None
    }

    /// Called every 50 ms. Returns true when a hardware START/PAUSE button
    /// (the pendant's) was pressed since the last call; the screen acts on
    /// it as on its own CYCLE START or FEED HOLD.
    fn tick(&self, _status: Option<&Status>) -> bool {
        false
    }

    /// After the HAL files ran. `has_file`: `ferrum/hal/<id>.hal` exists
    /// and was run. Returns problems for the message line.
    fn after_hal(&mut self, _has_file: bool) -> Vec<String> {
        Vec::new()
    }
}

/// Something a hardware panel or an alarm asks the screen for.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    CycleStart,
    FeedHold,
    Stop,
    SingleBlock,
    /// An override knob turned: "feed", "spindle" or "rapid", by n %.
    Override(&'static str, i32),
    /// A `kind = "stop"` alarm came on: feed hold, and say why.
    AlarmStop(String),
}

/// The screen's state for HAL outputs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outputs {
    /// E-stop, or a fault in the status list.
    pub error: bool,
    /// Paused, or waiting for the operator (M0/M1, a tool change).
    pub waiting: bool,
    pub running: bool,
    /// The axis last jogged on MANUAL, and the jog step (0 for continuous).
    pub mpg_axis: Option<char>,
    pub mpg_increment: f64,
}

/// Tool setter pins (ferrum/hal/probe.hal).
pub struct SetterHal {
    pub blast_request: Out<bool>,
    pub blast_active: In<bool>,
    pub triggered: In<bool>,
    /// The pins are connected (HAL file or `[hal]`): without them UTIL
    /// shows the setter as not wired and has no air blast.
    pub trip_wired: Cell<bool>,
    pub blast_wired: Cell<bool>,
}

/// Modules enabled when ferrum.ini does not list any. The mill-specific
/// ones (drives, drawbar, interlock, pendant, load) need HAL signals a stock
/// machine does not have, so they are only built when listed.
pub const DEFAULT_MODULES: &str = "run job manual probe tools offsets conv files util";

const ICON_RUN: &str = "M7 4l12 8-12 8z";
const ICON_JOB: &str = "M4 6h16M4 12h16M4 18h10";
const ICON_JOG: &str = "M12 3v18M3 12h18M12 3l-3 3M12 3l3 3M21 12l-3-3M21 12l-3 3";
const ICON_PROBE: &str = "M8 16A4 4 0 1 0 16 16A4 4 0 1 0 8 16M12 3v9";
const ICON_TOOLS: &str = "M9 3h6v6l-3 12-3-12z";
const ICON_OFFSETS: &str = "M4 20V4h16M12 10A2 2 0 1 0 16 10A2 2 0 1 0 12 10";
const ICON_CONV: &str = "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM8 9h8M8 13h8M8 17h5";
const ICON_FILES: &str = "M4 6h6l2 2h8v11H4z";
// A wrench.
const ICON_SETUP: &str = "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z";
const ICON_UTIL: &str =
    "M9 12A3 3 0 1 0 15 12A3 3 0 1 0 9 12M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M5 19l2-2M17 7l2-2";

/// A module that only contributes a screen (for now).
struct Screen {
    id: &'static str,
    label: &'static str,
    icon: &'static str,
}

impl Module for Screen {
    fn id(&self) -> &'static str {
        self.id
    }
    fn nav(&self) -> Option<NavEntry> {
        Some(NavEntry { id: self.id, label: self.label, icon: self.icon })
    }
}

/// Probe screen plus the probe status pill: from probe_manager on mymill
/// (`probe_loaded`, `probe_fault`), else from the tool number.
#[derive(Default)]
struct Probe {
    selected: Option<In<bool>>,
    fault: Option<In<bool>>,
    tripped: Option<In<bool>>,
    setter: Option<Rc<SetterHal>>,
    /// The probe's tool number, for machines with no `probe_loaded` signal.
    tool: i32,
    loaded_wired: bool,
    fault_wired: bool,
}

impl Module for Probe {
    fn id(&self) -> &'static str {
        "probe"
    }
    fn nav(&self) -> Option<NavEntry> {
        Some(NavEntry { id: "probe", label: "PROBE", icon: ICON_PROBE })
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.selected = Some(comp.pin("probe.wireless-selected")?);
        self.fault = Some(comp.pin("probe.fault")?);
        self.tripped = Some(comp.pin("probe.tripped")?);
        self.setter = Some(Rc::new(SetterHal {
            blast_request: comp.pin("probe.blast-request")?,
            blast_active: comp.pin("probe.blast-active")?,
            triggered: comp.pin("probe.toolsetter")?,
            trip_wired: Cell::new(false),
            blast_wired: Cell::new(false),
        }));
        Ok(())
    }
    fn linked(&mut self, name: &str) {
        match name {
            "probe_loaded" => self.loaded_wired = true,
            "probe_fault" => self.fault_wired = true,
            "toolsetter_tripped" => self.setter.iter().for_each(|h| h.trip_wired.set(true)),
            "setter_blast" => self.setter.iter().for_each(|h| h.blast_wired.set(true)),
            _ => {}
        }
    }
    fn pills(&self, s: Option<&Status>, out: &mut Vec<Pill>) {
        let (Some(sel), Some(fault), Some(trip)) = (&self.selected, &self.fault, &self.tripped) else { return };
        let loaded = if self.loaded_wired { sel.get() } else { s.is_some_and(|s| s.tool_in_spindle == self.tool) };
        out.push(if !loaded {
            Pill::new("PROBE NOT LOADED", "")
        } else if self.fault_wired && fault.get() {
            Pill::new("PROBE FAULT", "bad").hint("The probe reports a fault. Check the probe.")
        } else if trip.get() {
            Pill::new("PROBE TRIPPED", "info")
        } else {
            Pill::new("PROBE LOADED", "warn").hint("The spindle will not start with the probe in.")
        });
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &["probe"]
    }
    fn spindle_block(&self) -> Option<String> {
        let sel = self.selected.as_ref().filter(|_| self.loaded_wired)?;
        sel.get().then(|| "Remove the probe before starting the spindle".into())
    }
    fn setter(&self) -> Option<Rc<SetterHal>> {
        self.setter.clone()
    }
}

/// Servo drive readiness from HLFB, per axis (ferrum/hal/drives.hal).
struct Drives {
    axes: Vec<char>,
    ready: Vec<(char, In<bool>)>,
    faults: Vec<(char, In<bool>)>,
}

impl Module for Drives {
    fn id(&self) -> &'static str {
        "drives"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        for a in &self.axes {
            let l = a.to_ascii_lowercase();
            self.ready.push((*a, comp.pin(&format!("drives.ready-{l}"))?));
            self.faults.push((*a, comp.pin(&format!("drives.fault-{l}"))?));
        }
        Ok(())
    }
    fn pills(&self, s: Option<&Status>, out: &mut Vec<Pill>) {
        if self.ready.is_empty() {
            return;
        }
        let faulted: String = self.faults.iter().filter(|(_, p)| p.get()).map(|(a, _)| *a).collect();
        out.push(if !faulted.is_empty() {
            Pill::new(format!("DRIVE FAULT {faulted}"), "bad").hint("The drive of each axis named reports a fault.")
        } else if self.not_ready().is_empty() {
            Pill::new("DRIVES HLFB OK", "good")
        } else if s.is_some_and(|s| s.power_on()) {
            Pill::new(format!("DRIVES NOT READY {}", self.not_ready()), "warn").hint("The drives named have not reported ready.")
        } else {
            Pill::new("DRIVES OFF", "")
        });
    }
    fn motion_block(&self) -> Option<String> {
        // The rotary is often off the table with its drive unplugged, and
        // HAL does not need it to run: only X, Y and Z hold a start back.
        let waiting: String = self.not_ready().chars().filter(|a| *a != 'A').collect();
        (!waiting.is_empty()).then(|| format!("Wait for the {waiting} drive(s) to become ready"))
    }
}

impl Drives {
    /// Axes whose HLFB has not armed yet, e.g. "XZ".
    fn not_ready(&self) -> String {
        self.ready.iter().filter(|(_, p)| !p.get()).map(|(a, _)| *a).collect()
    }
}

/// Power drawbar (spindle_drawbar_guard) and the manual tool change that
/// uses it (crate::toolchange). The drawbar pins are commands, not sensors,
/// so the pill says CMD.
#[derive(Default)]
struct Drawbar {
    io: Option<Rc<crate::toolchange::HalIo>>,
}

impl Module for Drawbar {
    fn id(&self) -> &'static str {
        "drawbar"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        let extended = comp.pin("drawbar.extended")?;
        let rearm = comp.pin("drawbar.spindle-rearm-required")?;
        self.io = Some(Rc::new(crate::toolchange::HalIo::create(comp, extended, rearm)?));
        Ok(())
    }
    fn pills(&self, _s: Option<&Status>, out: &mut Vec<Pill>) {
        let Some(io) = &self.io else { return };
        out.push(if io.extended() { Pill::new("DRAWBAR CMD RELEASE", "bad").hint("The drawbar is told to let go of the tool.") } else { Pill::new("DRAWBAR CMD CLAMP", "") });
        if io.rearm_required() {
            out.push(Pill::new("SPINDLE: M5, THEN START AGAIN", "bad"));
        }
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &["drawbar"]
    }
    fn spindle_block(&self) -> Option<String> {
        let io = self.io.as_ref()?;
        let i = crate::toolchange::Io::read(io.as_ref());
        if i.manual_request {
            Some("Release the drawbar request before starting the spindle".into())
        } else if io.extended() {
            Some("Retract the drawbar before starting the spindle".into())
        } else if io.rearm_required() {
            Some("Stop the spindle command before restarting".into())
        } else {
            None
        }
    }
    fn toolchange(&self) -> Option<Rc<dyn crate::toolchange::Io>> {
        self.io.clone().map(|io| io as Rc<dyn crate::toolchange::Io>)
    }
}

/// The tool change dialog on a machine without the drawbar: M6 through
/// iocontrol, answered by the overlay (crate::toolchange::DialogIo).
#[derive(Default)]
struct ToolChange {
    io: Option<Rc<crate::toolchange::DialogIo>>,
}

impl Module for ToolChange {
    fn id(&self) -> &'static str {
        "toolchange"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.io = Some(Rc::new(crate::toolchange::DialogIo::create(comp)?));
        Ok(())
    }
    fn toolchange(&self) -> Option<Rc<dyn crate::toolchange::Io>> {
        self.io.clone().map(|io| io as Rc<dyn crate::toolchange::Io>)
    }
}

/// Why motion.feed-inhibit is set. In mymill.hal that is only the probe
/// manager's fault; drives-not-ready stays unlinked unless a HAL adds it.
#[derive(Default)]
struct Interlock {
    held: Option<In<bool>>,
    probe_fault: Option<In<bool>>,
    drives_not_ready: Option<In<bool>>,
}

impl Module for Interlock {
    fn id(&self) -> &'static str {
        "interlock"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.held = Some(comp.pin("interlock.held")?);
        self.probe_fault = Some(comp.pin("interlock.probe-fault")?);
        self.drives_not_ready = Some(comp.pin("interlock.drives-not-ready")?);
        Ok(())
    }
    fn pills(&self, s: Option<&Status>, out: &mut Vec<Pill>) {
        let Some(held) = &self.held else { return };
        // Drives are expected to be not ready while the machine is off.
        if !held.get() || !s.is_some_and(|s| s.power_on()) {
            return;
        }
        let mut why = Vec::new();
        if self.probe_fault.as_ref().is_some_and(|p| p.get()) {
            why.push("PROBE FAULT");
        }
        if self.drives_not_ready.as_ref().is_some_and(|p| p.get()) {
            why.push("DRIVES NOT READY");
        }
        let reason = if why.is_empty() { "UNKNOWN".to_string() } else { why.join(" + ") };
        out.push(Pill::new(format!("MOTION HELD: {reason}"), "bad").hint("Feed is held until this clears."));
    }
    fn motion_block(&self) -> Option<String> {
        self.held.as_ref().is_some_and(|p| p.get()).then(|| "Motion is held (probe fault or drives)".into())
    }
}

/// Cutting load for the backplot (ferrum/hal/load.hal). Opt-in: list
/// `load` in [FERRUM]MODULES once a load signal is netted to the pin.
/// `ferrum.load.value` is a float, 0 (idle) to 1 (full load); the HAL file
/// does the scaling, so any source works (spindle current, drive torque).
#[derive(Default)]
struct Load {
    value: Option<In<f64>>,
    /// Pins were asked for: this is the machine, not the simulator.
    real: bool,
}

impl Module for Load {
    fn id(&self) -> &'static str {
        "load"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.real = true;
        self.value = Some(comp.pin("load.value")?);
        Ok(())
    }
    fn load(&self, s: Option<&Status>) -> Option<f64> {
        match &self.value {
            Some(p) => Some(p.get().clamp(0.0, 1.0)),
            // On the machine with no pin there is no load to show.
            None if self.real => None,
            // The simulator: a stand-in that rises with the feed rate,
            // only so the colouring can be seen and tested.
            None => s.map(|s| if s.busy() { (s.feed_per_min() / 1200.0).clamp(0.0, 1.0) } else { 0.0 }),
        }
    }
}

/// The WHB04B pendant (ferrum/hal/pendant.hal, xhc-whb04b-6.hal). Its
/// START/PAUSE button comes to ferrum instead of LinuxCNC: halui would
/// switch to AUTO (which aborts a running MDI command or probe macro) and
/// skip CYCLE START's checks and its G90 setup. The pill says whether the
/// pendant is there.
#[derive(Default)]
struct Pendant {
    connected: Option<In<bool>>,
    asleep: Option<In<bool>>,
    /// whb.button.start-pause: the driver raises it when task is already in AUTO.
    start: Option<In<bool>>,
    /// whb.halui.mode.auto: otherwise the driver asks for AUTO first.
    auto_request: Option<In<bool>>,
    /// The driver's other mode requests (it asks for MANUAL or teleop on
    /// every wheel step, joint mode before HOME), passed on to halui only
    /// while nothing runs: a switch out of MDI aborts a probe macro.
    modes: Vec<(In<bool>, Out<bool>)>,
    was_down: Cell<bool>,
    last: Cell<Option<Instant>>,
}

/// One press can raise both pins one after the other.
const PENDANT_DEBOUNCE: Duration = Duration::from_millis(700);

/// Mode requests ferrum passes from the pendant to halui.
const PENDANT_MODES: [&str; 4] = ["manual", "teleop", "joint", "mdi"];

/// Whether a pendant mode request may reach halui now: nothing running,
/// queued or paused (a switch would abort it).
fn pendant_modes_ok(s: Option<&Status>) -> bool {
    s.is_some_and(|s| !s.busy() && s.interp == machine::Interp::Idle && s.queued_mdi == 0)
}

impl Pendant {
    /// Edge detection on the two pins, one press per PENDANT_DEBOUNCE.
    fn pressed(&self, down: bool, now: Instant) -> bool {
        let edge = down && !self.was_down.get();
        self.was_down.set(down);
        if !edge || self.last.get().is_some_and(|t| now.duration_since(t) < PENDANT_DEBOUNCE) {
            return false;
        }
        self.last.set(Some(now));
        true
    }
}

impl Module for Pendant {
    fn id(&self) -> &'static str {
        "pendant"
    }
    /// The pendant's START/PAUSE and mode requests come to ferrum instead
    /// of halui (see the struct): take halui's mode and run inputs off the
    /// driver's signals before ferrum's pins join them.
    fn before_links(&mut self) {
        let mapped = crate::machine_cfg::get().hal.keys().any(|k| k.starts_with("pendant_"));
        if !mapped && crate::halmap::pin_info("whb.halui.mode.auto").is_none() {
            return;
        }
        for p in ["halui.mode.auto", "halui.program.run", "halui.mode.manual", "halui.mode.teleop", "halui.mode.joint", "halui.mode.mdi"] {
            if let Err(e) = crate::halmap::halcmd(&["unlinkp", p]) {
                eprintln!("ferrum: pendant: unlinkp {p}: {e}");
            }
        }
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.connected = Some(comp.pin("pendant.connected")?);
        self.asleep = Some(comp.pin("pendant.asleep")?);
        self.start = Some(comp.pin("pendant.start")?);
        self.auto_request = Some(comp.pin("pendant.auto-request")?);
        for m in PENDANT_MODES {
            self.modes.push((comp.pin(&format!("pendant.ask-{m}"))?, comp.pin(&format!("pendant.mode-{m}"))?));
        }
        Ok(())
    }
    fn pills(&self, _s: Option<&Status>, out: &mut Vec<Pill>) {
        let Some(c) = &self.connected else { return };
        out.push(if !c.get() {
            Pill::new("PENDANT OFF", "")
        } else if self.asleep.as_ref().is_some_and(|p| p.get()) {
            Pill::new("PENDANT ASLEEP", "")
        } else {
            Pill::new("PENDANT", "good")
        });
    }
    fn tick(&self, s: Option<&Status>) -> bool {
        let ok = pendant_modes_ok(s);
        for (ask, out) in &self.modes {
            out.set(ask.get() && ok);
        }
        let down = [&self.start, &self.auto_request].iter().any(|p| p.as_ref().is_some_and(|p| p.get()));
        self.pressed(down, Instant::now())
    }
}

/// Machine signals every config can have (crate::halpins, always on):
/// spindle feedback, spindle inhibit and fault, lube level. Each shows only
/// when connected to something that drives it.
#[derive(Default)]
struct MachineSignals {
    speed: Option<In<f64>>,
    at_speed: Option<In<bool>>,
    inhibited: Option<In<bool>>,
    fault: Option<In<bool>>,
    lube_ok: Option<In<bool>>,
    wired: Vec<&'static str>,
    /// `[[alarm]]`s: (pin `alarm.<n>`, text, kind, connected, was on).
    alarms: Vec<(In<bool>, String, &'static str, Cell<bool>, Cell<bool>)>,
}

impl MachineSignals {
    fn has(&self, name: &str) -> bool {
        self.wired.contains(&name)
    }
}

impl Module for MachineSignals {
    fn id(&self) -> &'static str {
        "machine"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.speed = Some(comp.pin("spindle.speed-actual")?);
        self.at_speed = Some(comp.pin("spindle.at-speed")?);
        self.inhibited = Some(comp.pin("spindle.inhibited")?);
        self.fault = Some(comp.pin("spindle.fault")?);
        self.lube_ok = Some(comp.pin("machine.lube-ok")?);
        for (n, a) in crate::machine_cfg::get().alarm.iter().enumerate() {
            let kind = match a.kind.as_deref() {
                Some("info") => "info",
                Some("stop") => "stop",
                _ => "warn",
            };
            self.alarms.push((comp.pin(&format!("alarm.{}", n + 1))?, a.text.clone(), kind, Cell::new(false), Cell::new(false)));
        }
        Ok(())
    }
    fn after_hal(&mut self, _has_file: bool) -> Vec<String> {
        let mut notes = Vec::new();
        for (n, a) in crate::machine_cfg::get().alarm.iter().enumerate() {
            let t = crate::machine_cfg::HalTarget::named(&a.pin);
            match crate::halmap::link_pin(&format!("alarm.{}", n + 1), crate::halpins::Ty::Bit, true, &t, true) {
                Ok(crate::halmap::Linked::Yes(how)) => {
                    eprintln!("ferrum: HAL {how}");
                    if let Some(x) = self.alarms.get(n) {
                        x.3.set(true);
                    }
                }
                Ok(crate::halmap::Linked::Nothing(why)) => notes.push(format!("[[alarm]] {}: {why}", a.text)),
                Err(e) => notes.push(format!("[[alarm]] {}: {e}", a.text)),
            }
        }
        notes
    }
    fn events(&self, _s: Option<&Status>) -> Vec<Event> {
        let mut v = Vec::new();
        for (pin, text, kind, wired, was) in &self.alarms {
            let on = wired.get() && pin.get();
            if on && !was.get() && *kind == "stop" {
                v.push(Event::AlarmStop(text.clone()));
            }
            was.set(on);
        }
        v
    }
    fn motion_block(&self) -> Option<String> {
        self.alarms.iter().find(|(p, _, k, w, _)| *k == "stop" && w.get() && p.get()).map(|(_, t, ..)| t.clone())
    }
    fn linked(&mut self, name: &str) {
        if let Some(d) = crate::halpins::by_name(name).find(|d| d.modules.contains(&"machine")) {
            self.wired.push(d.name.unwrap_or(""));
        }
    }
    fn pills(&self, s: Option<&Status>, out: &mut Vec<Pill>) {
        let on = |p: &Option<In<bool>>, n: &str| self.has(n) && p.as_ref().is_some_and(|p| p.get());
        if self.has("spindle_fault") {
            out.push(if on(&self.fault, "spindle_fault") {
                Pill::new("SPINDLE FAULT", "bad").hint("The spindle drive reports a fault. Check the VFD.")
            } else {
                Pill::new("SPINDLE DRIVE OK", "good")
            });
        }
        if on(&self.inhibited, "spindle_inhibited") && s.is_some_and(|s| s.power_on()) {
            out.push(Pill::new("SPINDLE INHIBITED", "warn").hint("HAL holds the spindle off (spindle.0.inhibit)."));
        }
        for (pin, text, kind, wired, _) in &self.alarms {
            if wired.get() && pin.get() {
                let k = if *kind == "stop" { "bad" } else { kind };
                out.push(Pill::new(text.to_uppercase(), k).hint("An alarm input of the machine ([[alarm]] in ferrum.toml)."));
            }
        }
        if self.has("lube_ok") {
            let ok = self.lube_ok.as_ref().is_some_and(|p| p.get());
            out.push(if ok { Pill::new("LUBE OK", "good") } else { Pill::new("LUBE LOW", "warn").hint("The lube level input says low. Fill the lube tank.") });
        }
    }
    fn spindle_block(&self) -> Option<String> {
        if self.has("spindle_fault") && self.fault.as_ref().is_some_and(|p| p.get()) {
            return Some("The spindle drive reports a fault".into());
        }
        None
    }
    fn spindle_feedback(&self) -> Option<(Option<f64>, Option<bool>)> {
        let rpm = self.speed.as_ref().filter(|_| self.has("spindle_speed_actual")).map(|p| (p.get() * 60.0).abs());
        let at = self.at_speed.as_ref().filter(|_| self.has("spindle_at_speed")).map(|p| p.get());
        (rpm.is_some() || at.is_some()).then_some((rpm, at))
    }
}

/// Hardware buttons, override knobs, an MPG that follows MANUAL, a stack
/// light and a settings key switch (crate::halpins, `panel.*`, `mpg.*`,
/// `status.*`). Always on; each part works once `[hal]` names its signal.
#[derive(Default)]
struct Panel {
    buttons: Vec<(&'static str, In<bool>, Cell<bool>)>,
    knobs: Vec<(&'static str, In<i32>, Cell<Option<i32>>)>,
    unlock: Option<In<bool>>,
    mpg_axes: Vec<(char, Out<bool>)>,
    mpg_increment: Option<Out<f64>>,
    lights: Vec<Out<bool>>,
    wired: Vec<&'static str>,
}

const PANEL_BUTTONS: [(&str, &str); 4] = [("cycle_start", "panel.cycle-start"), ("feed_hold", "panel.feed-hold"), ("stop", "panel.stop"), ("single_block", "panel.single-block")];
const PANEL_KNOBS: [(&str, &str, &str); 3] =
    [("feed", "feed_override_counts", "panel.feed-override-counts"), ("spindle", "spindle_override_counts", "panel.spindle-override-counts"), ("rapid", "rapid_override_counts", "panel.rapid-override-counts")];

impl Module for Panel {
    fn id(&self) -> &'static str {
        "panel"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        for (name, pin) in PANEL_BUTTONS {
            self.buttons.push((name, comp.pin(pin)?, Cell::new(false)));
        }
        for (which, _, pin) in PANEL_KNOBS {
            self.knobs.push((which, comp.pin(pin)?, Cell::new(None)));
        }
        self.unlock = Some(comp.pin("panel.unlock")?);
        for a in "XYZABC".chars() {
            self.mpg_axes.push((a, comp.pin(&format!("mpg.axis-{}", a.to_ascii_lowercase()))?));
        }
        self.mpg_increment = Some(comp.pin("mpg.increment")?);
        for p in ["status.error", "status.waiting", "status.running"] {
            self.lights.push(comp.pin(p)?);
        }
        Ok(())
    }
    fn linked(&mut self, name: &str) {
        if let Some(d) = crate::halpins::by_name(name).find(|d| d.modules.contains(&"panel")) {
            self.wired.push(d.name.unwrap_or(""));
        }
    }
    fn events(&self, _s: Option<&Status>) -> Vec<Event> {
        let mut v = Vec::new();
        for (name, pin, was) in &self.buttons {
            let down = pin.get();
            if down && !was.get() && self.wired.contains(name) {
                v.push(match *name {
                    "cycle_start" => Event::CycleStart,
                    "feed_hold" => Event::FeedHold,
                    "stop" => Event::Stop,
                    _ => Event::SingleBlock,
                });
            }
            was.set(down);
        }
        for ((which, name, _), (_, pin, last)) in PANEL_KNOBS.iter().zip(&self.knobs) {
            let now = pin.get();
            if let Some(before) = last.get() {
                let d = now.wrapping_sub(before);
                if d != 0 && self.wired.contains(name) {
                    v.push(Event::Override(which, d.clamp(-100, 100)));
                }
            }
            last.set(Some(now));
        }
        v
    }
    fn outputs(&self, o: &Outputs) {
        for (a, pin) in &self.mpg_axes {
            pin.set(o.mpg_axis == Some(*a));
        }
        if let Some(p) = &self.mpg_increment {
            p.set(o.mpg_increment);
        }
        for (pin, on) in self.lights.iter().zip([o.error, o.waiting && !o.error, o.running && !o.waiting && !o.error]) {
            pin.set(on);
        }
    }
    fn locked(&self) -> Option<bool> {
        self.wired.contains(&"unlock").then(|| !self.unlock.as_ref().is_some_and(|p| p.get()))
    }
    fn pills(&self, _s: Option<&Status>, out: &mut Vec<Pill>) {
        if self.locked() == Some(true) {
            out.push(Pill::new("SETUP LOCKED", "").hint("The key switch locks SETUP. Turn the key to change settings."));
        }
    }
}

fn make(id: &str, cfg: &Config) -> Option<Box<dyn Module>> {
    let screen = |id, label, icon| Some(Box::new(Screen { id, label, icon }) as Box<dyn Module>);
    match id {
        "run" => screen("run", "RUN", ICON_RUN),
        "job" => screen("job", "JOB", ICON_JOB),
        // "jog" is the old name of MANUAL; keep old INIs working.
        "manual" | "jog" => screen("manual", "MANUAL", ICON_JOG),
        "tools" => screen("tools", "TOOLS", ICON_TOOLS),
        "offsets" => screen("offsets", "OFFSETS", ICON_OFFSETS),
        "conv" => screen("conv", "CONV", ICON_CONV),
        "files" => screen("files", "FILES", ICON_FILES),
        "util" => screen("util", "UTIL", ICON_UTIL),
        "setup" => screen("setup", "SETUP", ICON_SETUP),
        "probe" => Some(Box::new(Probe { tool: crate::machine_cfg::get().probe_tool(cfg), ..Probe::default() })),
        "toolchange" => Some(Box::new(ToolChange::default())),
        "drives" => Some(Box::new(Drives { axes: cfg.coordinates().chars().collect(), ready: Vec::new(), faults: Vec::new() })),
        "drawbar" => Some(Box::new(Drawbar::default())),
        "interlock" => Some(Box::new(Interlock::default())),
        "load" => Some(Box::new(Load::default())),
        "pendant" => Some(Box::new(Pendant::default())),
        "watchdog" => Some(Box::new(Watchdog::default())),
        "machine" => Some(Box::new(MachineSignals::default())),
        "panel" => Some(Box::new(Panel::default())),
        _ => None,
    }
}

/// The screen watchdog (failsafe.rs): ferrum flips `watchdog.heartbeat`
/// per drawn frame; a realtime oneshot trips E-stop when it stops. Always
/// on. `ferrum/hal/watchdog.hal` wires it for the mymill profile; without
/// that file ferrum loads the oneshot itself and trips `[hal]
/// screen_watchdog_trip` (an unconnected bit input such as
/// `estop-latch.0.fault-in`, driven TRUE when the screen is stale) or, by
/// default, gates `iocontrol.0.emc-enable-in`, LinuxCNC's E-stop input.
#[derive(Default)]
struct Watchdog {
    ok: Option<In<bool>>,
    wired: Cell<bool>,
    /// Turned off in ferrum.toml.
    off: Cell<bool>,
}

/// The HAL instances the built-in wiring loads.
const WD_ONESHOT: &str = "ferrum-screen-watchdog";
const WD_GATE: &str = "ferrum-screen-gate";
const WD_LATCH: &str = "ferrum-screen-latch";
const WD_ENABLE: &str = "iocontrol.0.emc-enable-in";
const WD_REQUEST: &str = "iocontrol.0.user-request-enable";

impl Module for Watchdog {
    fn id(&self) -> &'static str {
        "watchdog"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        let beat: Out<bool> = comp.pin("watchdog.heartbeat")?;
        crate::failsafe::set_pin(beat);
        self.ok = Some(comp.pin("watchdog.ok")?);
        Ok(())
    }
    fn after_hal(&mut self, has_file: bool) -> Vec<String> {
        if self.ok.is_none() {
            return Vec::new();
        }
        if !crate::machine_cfg::get().screen_watchdog() {
            eprintln!("ferrum: screen watchdog off ([machine]screen_watchdog = false)");
            self.off.set(true);
            return Vec::new();
        }
        let r = if has_file { check_watchdog_file() } else { wire_watchdog() };
        match r {
            Ok(how) => {
                eprintln!("ferrum: screen watchdog: {how}");
                self.wired.set(true);
                Vec::new()
            }
            Err(e) => vec![format!("Screen watchdog not wired: {e}")],
        }
    }
    fn pills(&self, s: Option<&Status>, out: &mut Vec<Pill>) {
        let Some(ok) = &self.ok else { return };
        if self.off.get() {
            out.push(Pill::new("SCREEN WATCHDOG OFF", "").hint("Turned off in SETUP › MACHINE. Recommended on: it E-stops the machine if this screen freezes."));
            return;
        }
        if ok.get() {
            crate::failsafe::arm();
        }
        // After a freeze the screen is live again but the machine stays in
        // E-stop: say why until E-stop is reset.
        if let Some(gap) = crate::failsafe::froze() {
            if s.is_some_and(|s| s.estop()) {
                out.push(
                    Pill::new(format!("SCREEN FROZE {:.1} S: E-STOP", gap.as_secs_f64()), "bad")
                        .hint("The screen stopped drawing for over a second, so HAL E-stopped the machine. Reset E-stop to go on."),
                );
                return;
            }
            crate::failsafe::clear_froze();
        }
        out.push(if !self.wired.get() {
            Pill::new("NO SCREEN WATCHDOG", "bad").hint("HAL does not stop the machine if this screen freezes. See the message line.")
        } else if !ok.get() {
            Pill::new("SCREEN WATCHDOG", "bad").hint("The screen stopped drawing for over a second, so HAL E-stopped the machine. Reset E-stop to go on.")
        } else {
            Pill::new("SCREEN WATCHDOG OK", "good")
        });
    }
}

/// The module's HAL file ran: its heartbeat must reach a timer, and the
/// timer must reach something that stops the machine. (halcmd runs the
/// file with -k, so a missing `estop-latch.0` on this machine only fails
/// that line, and the heartbeat alone would look wired.)
fn check_watchdog_file() -> Result<String, String> {
    let all = crate::halmap::snapshot().ok_or("HAL did not answer")?;
    watchdog_trip(&all)
}

/// From the whole HAL: the heartbeat's signal goes into a component (the
/// oneshot) one of whose outputs drives a pin outside ferrum.
fn watchdog_trip(all: &[crate::halmap::HalItem]) -> Result<String, String> {
    let pin = |n: &str| all.iter().find(|i| !i.is_signal() && i.name == n);
    let beat = pin("ferrum.watchdog.heartbeat").map(|i| i.sig.clone()).unwrap_or_default();
    if beat.is_empty() {
        return Err("ferrum/hal/watchdog.hal left ferrum.watchdog.heartbeat unconnected".into());
    }
    let outside_readers = |sig: &str| -> Vec<&str> {
        all.iter().filter(|i| !i.is_signal() && i.dir == "IN" && i.sig == sig && !i.name.starts_with("ferrum.")).map(|i| i.name.as_str()).collect()
    };
    for timer in outside_readers(&beat) {
        let Some((comp, _)) = timer.rsplit_once('.') else { continue };
        let outs = all.iter().filter(|i| !i.is_signal() && i.dir == "OUT" && !i.sig.is_empty() && i.name.rsplit_once('.').is_some_and(|(c, _)| c == comp));
        for out in outs {
            if let Some(trip) = outside_readers(&out.sig).first() {
                return Ok(format!("ferrum/hal/watchdog.hal (heartbeat on {beat}, trips {trip})"));
            }
        }
    }
    Err(format!("ferrum/hal/watchdog.hal: the heartbeat ({beat}) reaches no timer that trips E-stop (is estop-latch.0 in the machine's HAL?)"))
}

/// No HAL file: load and connect the oneshot, then the trip.
fn wire_watchdog() -> Result<String, String> {
    use crate::halmap::{halcmd, pin_info};
    let o = WD_ONESHOT;
    let width = format!("{}", crate::failsafe::TIMEOUT);
    for cmd in [
        vec!["loadrt", "oneshot", &format!("names={o}")],
        vec!["addf", o, "servo-thread"],
        vec!["setp", &format!("{o}.width"), &width],
        vec!["setp", &format!("{o}.retriggerable"), "1"],
        vec!["setp", &format!("{o}.rising"), "1"],
        vec!["setp", &format!("{o}.falling"), "1"],
        vec!["net", "ferrum-screen-heartbeat", "ferrum.watchdog.heartbeat", &format!("{o}.in")],
        vec!["net", "ferrum-screen-ok", &format!("{o}.out"), "ferrum.watchdog.ok"],
    ] {
        halcmd(&cmd).map_err(|e| format!("halcmd {}: {e}", cmd.join(" ")))?;
    }
    if let Some(t) = crate::machine_cfg::get().hal.get(crate::halpins::WATCHDOG_TRIP).and_then(|t| t.target.clone()) {
        halcmd(&["net", "ferrum-screen-stale", &format!("{o}.out-not"), &t]).map_err(|e| format!("[hal]screen_watchdog_trip: {e}"))?;
        return Ok(format!("trips {t}"));
    }
    // Gate LinuxCNC's E-stop input: enable = (what drove it) AND screen ok,
    // latched: a stale screen holds the gate shut until the operator's next
    // E-stop reset (iocontrol's user-request-enable), as an E-stop button
    // would. (An unlatched gate lets LinuxCNC leave E-stop by itself when
    // the screen comes back.)
    let (_, sig) = pin_info(WD_ENABLE).ok_or_else(|| format!("HAL has no {WD_ENABLE}"))?;
    let (g, l) = (WD_GATE, WD_LATCH);
    halcmd(&["loadrt", "estop_latch", &format!("names={l}")])?;
    halcmd(&["addf", l, "servo-thread"])?;
    halcmd(&["setp", &format!("{l}.ok-in"), "1"])?;
    halcmd(&["net", "ferrum-screen-stale", &format!("{o}.out-not"), &format!("{l}.fault-in")])?;
    let (_, req) = pin_info(WD_REQUEST).ok_or_else(|| format!("HAL has no {WD_REQUEST}"))?;
    if req.is_empty() {
        halcmd(&["net", "ferrum-screen-reset", WD_REQUEST, &format!("{l}.reset")])?;
    } else {
        halcmd(&["net", &req, &format!("{l}.reset")])?;
    }
    halcmd(&["loadrt", "and2", &format!("names={g}")])?;
    halcmd(&["addf", g, "servo-thread"])?;
    if sig.is_empty() {
        halcmd(&["setp", &format!("{g}.in0"), "1"])?;
    } else {
        halcmd(&["unlinkp", WD_ENABLE])?;
        halcmd(&["net", &sig, &format!("{g}.in0")])?;
    }
    halcmd(&["net", "ferrum-screen-latched-ok", &format!("{l}.ok-out"), &format!("{g}.in1")])?;
    halcmd(&["net", "ferrum-screen-enable", &format!("{g}.out"), WD_ENABLE])?;
    Ok(format!("gates {WD_ENABLE} (was {})", if sig.is_empty() { "unconnected" } else { &sig }))
}

/// Build the enabled modules in the order listed. Unknown ids are reported,
/// not fatal, so a typo cannot keep the machine screen from starting.
/// With no module answering M6 (`drawbar`, `toolchange`) and nothing in the
/// machine's HAL answering it either, the `toolchange` module is added.
pub fn build(cfg: &Config, hal: &dyn crate::checks::Hal) -> (Vec<Box<dyn Module>>, Vec<String>) {
    let mc = crate::machine_cfg::get();
    let mut list = mc.module_ids(cfg, DEFAULT_MODULES);
    // `[probe]enabled = false`: no probe on this machine.
    if mc.probe.enabled == Some(false) {
        list.retain(|m| m != "probe");
    }
    if !list.iter().any(|m| m == "drawbar" || m == "toolchange")
        && crate::checks::toolchange_style(mc, &list, hal).0 == crate::machine_cfg::ToolChangeStyle::Dialog
    {
        list.push("toolchange".into());
    }
    // SETUP is always there, last. The screen watchdog and the machine
    // signals are always there.
    list.retain(|m| !crate::halpins::ALWAYS_ON.contains(&m.as_str()));
    list.push("setup".into());
    list.push("watchdog".into());
    list.push("machine".into());
    list.push("panel".into());
    let mut modules: Vec<Box<dyn Module>> = Vec::new();
    let mut problems = Vec::new();
    for id in list.iter().map(String::as_str) {
        if modules.iter().any(|m| m.id() == id) {
            continue;
        }
        match make(id, cfg) {
            Some(m) => modules.push(m),
            None => problems.push(format!("unknown module '{id}' in the module list")),
        }
    }
    (modules, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pendant_press_counts_once() {
        let p = Pendant::default();
        let t0 = Instant::now();
        assert!(p.pressed(true, t0));
        // Held, then the second pin of the same press.
        assert!(!p.pressed(true, t0 + Duration::from_millis(50)));
        assert!(!p.pressed(false, t0 + Duration::from_millis(100)));
        assert!(!p.pressed(true, t0 + Duration::from_millis(300)));
        assert!(!p.pressed(false, t0 + Duration::from_millis(400)));
        // A new press later.
        assert!(p.pressed(true, t0 + Duration::from_millis(1500)));
    }

    #[test]
    fn pendant_modes_pass_only_while_idle() {
        let mut s = Status::default();
        assert!(!pendant_modes_ok(None));
        assert!(pendant_modes_ok(Some(&s)));
        s.interp = machine::Interp::Reading;
        assert!(!pendant_modes_ok(Some(&s)));
        s.interp = machine::Interp::Paused;
        assert!(!pendant_modes_ok(Some(&s)));
    }

    #[test]
    fn unlisted_modules_do_not_exist() {
        let d = std::env::temp_dir().join(format!("ferrum-mod-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("m.ini"), "[FERRUM]\nMODULES = run jog bogus\n").unwrap();
        let cfg = Config::load(&d.join("m.ini")).unwrap();
        let (mods, problems) = build(&cfg, &crate::checks::NoHal);
        let ids: Vec<_> = mods.iter().map(|m| m.id()).collect();
        // Nothing answers M6, so the tool change dialog comes too; the
        // screen watchdog is always there.
        assert_eq!(ids, ["run", "manual", "toolchange", "setup", "watchdog", "machine", "panel"]);
        assert_eq!(problems.len(), 1);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn the_watchdog_file_must_reach_an_estop() {
        use crate::halmap::parse_all;
        let pins = "ferrum bit OUT TRUE ferrum.watchdog.heartbeat ==> ferrum-screen-heartbeat\n\
                    ferrum bit IN TRUE ferrum.watchdog.ok <== ferrum-screen-ok\n\
                    oneshot bit IN TRUE ferrum-screen-watchdog.in <== ferrum-screen-heartbeat\n\
                    oneshot bit OUT TRUE ferrum-screen-watchdog.out ==> ferrum-screen-ok\n\
                    oneshot bit OUT FALSE ferrum-screen-watchdog.out-not ==> ferrum-screen-stale\n";
        let sigs = "bit TRUE ferrum-screen-heartbeat\nbit TRUE ferrum-screen-ok\nbit FALSE ferrum-screen-stale\n";
        // estop-latch.0 missing: the net to its fault-in failed, nothing trips.
        let e = watchdog_trip(&parse_all(pins, sigs)).unwrap_err();
        assert!(e.contains("reaches no timer"), "{e}");
        let wired = format!("{pins}estop_latch bit IN FALSE estop-latch.0.fault-in <== ferrum-screen-stale\n");
        let ok = watchdog_trip(&parse_all(&wired, sigs)).unwrap();
        assert!(ok.contains("trips estop-latch.0.fault-in"), "{ok}");
        let loose = "ferrum bit OUT TRUE ferrum.watchdog.heartbeat\n";
        assert!(watchdog_trip(&parse_all(loose, "")).unwrap_err().contains("unconnected"));
    }
}
