//! Feature modules. Each machine capability or screen is one module; the
//! `[MILLGUI]MODULES` list in millgui.ini decides which exist. A module that
//! is not listed is never constructed: no nav entry, no HAL pins, no postgui
//! nets (`millgui/hal/<id>.hal`), no status pills.

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
    /// `millgui.<id>.<pin>` and netted by `millgui/hal/<id>.hal`.
    fn create_pins(&mut self, _comp: &Component) -> Result<(), String> {
        Ok(())
    }

    /// Pins that connect without a HAL file: `[hal]<name>` or the default
    /// target (crate::halmap). Ignored when `millgui/hal/<id>.hal` exists.
    fn hal_links(&self) -> &'static [crate::halmap::Link] {
        &[]
    }

    /// A pin of `hal_links` was connected (or the module's HAL file ran).
    fn linked(&mut self, _name: &str) {}

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
}

/// Tool setter pins (millgui/hal/probe.hal).
pub struct SetterHal {
    pub blast_request: Out<bool>,
    pub blast_active: In<bool>,
    pub triggered: In<bool>,
    /// The pins are connected (HAL file or `[hal]`): without them UTIL
    /// shows the setter as not wired and has no air blast.
    pub trip_wired: Cell<bool>,
    pub blast_wired: Cell<bool>,
}

/// Modules enabled when millgui.ini does not list any. The mill-specific
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

use crate::halmap::Link;

const PROBE_LINKS: &[Link] = &[
    Link { name: "probe_tripped", pin: "probe.tripped", input: true, auto: Some("motion.probe-input") },
    Link { name: "probe_loaded", pin: "probe.wireless-selected", input: true, auto: None },
    Link { name: "probe_fault", pin: "probe.fault", input: true, auto: None },
    Link { name: "toolsetter_tripped", pin: "probe.toolsetter", input: true, auto: None },
    Link { name: "setter_blast", pin: "probe.blast-request", input: false, auto: None },
    Link { name: "setter_blast_active", pin: "probe.blast-active", input: true, auto: None },
];

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
    fn hal_links(&self) -> &'static [Link] {
        PROBE_LINKS
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

/// Servo drive readiness from HLFB, per axis (millgui/hal/drives.hal).
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
    fn hal_links(&self) -> &'static [Link] {
        &[
            Link { name: "tool_change", pin: "toolchange.tool-change", input: true, auto: Some("iocontrol.0.tool-change") },
            Link { name: "tool_number", pin: "toolchange.tool-number", input: true, auto: Some("iocontrol.0.tool-prep-number") },
            Link { name: "tool_changed", pin: "toolchange.tool-changed", input: false, auto: Some("iocontrol.0.tool-changed") },
            Link { name: "spindle_on", pin: "toolchange.spindle-on", input: true, auto: Some("spindle.0.on") },
        ]
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
    fn hal_links(&self) -> &'static [Link] {
        &[
            Link { name: "motion_held", pin: "interlock.held", input: true, auto: Some("motion.feed-inhibit") },
            Link { name: "probe_fault", pin: "interlock.probe-fault", input: true, auto: None },
            Link { name: "drives_not_ready", pin: "interlock.drives-not-ready", input: true, auto: None },
        ]
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

/// Cutting load for the backplot (millgui/hal/load.hal). Opt-in: list
/// `load` in [MILLGUI]MODULES once a load signal is netted to the pin.
/// `millgui.load.value` is a float, 0 (idle) to 1 (full load); the HAL file
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
    fn hal_links(&self) -> &'static [Link] {
        &[Link { name: "load", pin: "load.value", input: true, auto: None }]
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

/// The WHB04B pendant (millgui/hal/pendant.hal, xhc-whb04b-6.hal). Its
/// START/PAUSE button comes to millgui instead of LinuxCNC: halui would
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

/// Mode requests millgui passes from the pendant to halui.
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
        _ => None,
    }
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
    // SETUP is always there, last.
    list.retain(|m| m != "setup");
    list.push("setup".into());
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
        let d = std::env::temp_dir().join(format!("millgui-mod-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("m.ini"), "[MILLGUI]\nMODULES = run jog bogus\n").unwrap();
        let cfg = Config::load(&d.join("m.ini")).unwrap();
        let (mods, problems) = build(&cfg, &crate::checks::NoHal);
        let ids: Vec<_> = mods.iter().map(|m| m.id()).collect();
        // Nothing answers M6, so the tool change dialog comes too.
        assert_eq!(ids, ["run", "manual", "toolchange", "setup"]);
        assert_eq!(problems.len(), 1);
        std::fs::remove_dir_all(d).ok();
    }
}
