//! Feature modules. Each machine capability or screen is one module; the
//! `[MILLGUI]MODULES` list in millgui.ini decides which exist. A module that
//! is not listed is never constructed: no nav entry, no HAL pins, no postgui
//! nets (`millgui/hal/<id>.hal`), no status pills.

use crate::config::Config;
use hal::{BitPin, Component, Dir};
use linuxcnc::Status;
use std::rc::Rc;

pub struct Pill {
    pub text: String,
    /// "" | "good" | "warn" | "bad" | "info"
    pub kind: &'static str,
}

impl Pill {
    pub fn new(text: impl Into<String>, kind: &'static str) -> Self {
        Self { text: text.into(), kind }
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

    /// Status bar pills, refreshed several times a second.
    fn pills(&self, _status: Option<&Status>, _out: &mut Vec<Pill>) {}

    /// Why GUI-started motion should not begin now, if this module knows.
    /// A preflight for clear messages only; HAL holds motion regardless.
    fn motion_block(&self) -> Option<String> {
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
}

/// Tool setter pins (millgui/hal/probe.hal).
pub struct SetterHal {
    pub blast_request: BitPin,
    pub blast_active: BitPin,
    pub triggered: BitPin,
}

/// Modules enabled when millgui.ini does not list any.
pub const DEFAULT_MODULES: &str = "run job manual probe tools offsets conv files util drives drawbar interlock";

const ICON_RUN: &str = "M7 4l12 8-12 8z";
const ICON_JOB: &str = "M4 6h16M4 12h16M4 18h10";
const ICON_JOG: &str = "M12 3v18M3 12h18M12 3l-3 3M12 3l3 3M21 12l-3-3M21 12l-3 3";
const ICON_PROBE: &str = "M8 16A4 4 0 1 0 16 16A4 4 0 1 0 8 16M12 3v9";
const ICON_TOOLS: &str = "M9 3h6v6l-3 12-3-12z";
const ICON_OFFSETS: &str = "M4 20V4h16M12 10A2 2 0 1 0 16 10A2 2 0 1 0 12 10";
const ICON_CONV: &str = "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM8 9h8M8 13h8M8 17h5";
const ICON_FILES: &str = "M4 6h6l2 2h8v11H4z";
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

/// Probe screen plus the probe status pill from probe_manager.
#[derive(Default)]
struct Probe {
    selected: Option<BitPin>,
    fault: Option<BitPin>,
    tripped: Option<BitPin>,
    setter: Option<Rc<SetterHal>>,
}

impl Module for Probe {
    fn id(&self) -> &'static str {
        "probe"
    }
    fn nav(&self) -> Option<NavEntry> {
        Some(NavEntry { id: "probe", label: "PROBE", icon: ICON_PROBE })
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.selected = Some(comp.bit("probe.wireless-selected", Dir::In)?);
        self.fault = Some(comp.bit("probe.fault", Dir::In)?);
        self.tripped = Some(comp.bit("probe.tripped", Dir::In)?);
        self.setter = Some(Rc::new(SetterHal {
            blast_request: comp.bit("probe.blast-request", Dir::Out)?,
            blast_active: comp.bit("probe.blast-active", Dir::In)?,
            triggered: comp.bit("probe.toolsetter", Dir::In)?,
        }));
        Ok(())
    }
    fn pills(&self, _s: Option<&Status>, out: &mut Vec<Pill>) {
        let (Some(sel), Some(fault), Some(trip)) = (&self.selected, &self.fault, &self.tripped) else { return };
        out.push(if !sel.get() {
            Pill::new("PROBE NOT LOADED", "")
        } else if fault.get() {
            Pill::new("PROBE FAULT", "bad")
        } else if trip.get() {
            Pill::new("PROBE TRIPPED", "info")
        } else {
            Pill::new("PROBE T99 · WIRED", "warn")
        });
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &["probe"]
    }
    fn setter(&self) -> Option<Rc<SetterHal>> {
        self.setter.clone()
    }
}

/// Servo drive readiness from drive_readiness (HLFB).
struct Drives {
    axes: Vec<char>,
    ready: Option<BitPin>,
    faults: Vec<(char, BitPin)>,
}

impl Module for Drives {
    fn id(&self) -> &'static str {
        "drives"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.ready = Some(comp.bit("drives.ready", Dir::In)?);
        for a in &self.axes {
            let name = format!("drives.fault-{}", a.to_ascii_lowercase());
            self.faults.push((*a, comp.bit(&name, Dir::In)?));
        }
        Ok(())
    }
    fn pills(&self, s: Option<&Status>, out: &mut Vec<Pill>) {
        let Some(ready) = &self.ready else { return };
        let faulted: String = self.faults.iter().filter(|(_, p)| p.get()).map(|(a, _)| *a).collect();
        out.push(if !faulted.is_empty() {
            Pill::new(format!("DRIVE FAULT {faulted}"), "bad")
        } else if ready.get() {
            Pill::new("DRIVES HLFB OK", "good")
        } else if s.is_some_and(|s| s.power_on()) {
            Pill::new("DRIVES NOT READY", "warn")
        } else {
            Pill::new("DRIVES OFF", "")
        });
    }
    fn motion_block(&self) -> Option<String> {
        let ready = self.ready.as_ref()?;
        (!ready.get()).then(|| "Wait for all drives to become ready".into())
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
        let extended = comp.bit("drawbar.extended", Dir::In)?;
        let rearm = comp.bit("drawbar.spindle-rearm-required", Dir::In)?;
        self.io = Some(Rc::new(crate::toolchange::HalIo::create(comp, extended, rearm)?));
        Ok(())
    }
    fn pills(&self, _s: Option<&Status>, out: &mut Vec<Pill>) {
        let Some(io) = &self.io else { return };
        out.push(if io.extended() { Pill::new("DRAWBAR CMD RELEASE", "bad") } else { Pill::new("DRAWBAR CMD CLAMP", "") });
        if io.rearm_required() {
            out.push(Pill::new("SPINDLE: M5, THEN START AGAIN", "bad"));
        }
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &["drawbar"]
    }
    fn toolchange(&self) -> Option<Rc<dyn crate::toolchange::Io>> {
        self.io.clone().map(|io| io as Rc<dyn crate::toolchange::Io>)
    }
}

/// motion.feed-inhibit = probe_manager fault OR drives not ready. Shows why
/// motion is held.
#[derive(Default)]
struct Interlock {
    held: Option<BitPin>,
    probe_fault: Option<BitPin>,
    drives_not_ready: Option<BitPin>,
}

impl Module for Interlock {
    fn id(&self) -> &'static str {
        "interlock"
    }
    fn create_pins(&mut self, comp: &Component) -> Result<(), String> {
        self.held = Some(comp.bit("interlock.held", Dir::In)?);
        self.probe_fault = Some(comp.bit("interlock.probe-fault", Dir::In)?);
        self.drives_not_ready = Some(comp.bit("interlock.drives-not-ready", Dir::In)?);
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
        out.push(Pill::new(format!("MOTION HELD: {reason}"), "bad"));
    }
    fn motion_block(&self) -> Option<String> {
        self.held.as_ref().is_some_and(|p| p.get()).then(|| "Motion is held (probe fault or drives)".into())
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
        "probe" => Some(Box::new(Probe::default())),
        "drives" => Some(Box::new(Drives { axes: cfg.coordinates().chars().collect(), ready: None, faults: Vec::new() })),
        "drawbar" => Some(Box::new(Drawbar::default())),
        "interlock" => Some(Box::new(Interlock::default())),
        _ => None,
    }
}

/// Build the enabled modules in the order listed. Unknown ids are reported,
/// not fatal, so a typo cannot keep the machine screen from starting.
pub fn build(cfg: &Config) -> (Vec<Box<dyn Module>>, Vec<String>) {
    let list = cfg.get("MILLGUI", "MODULES").unwrap_or(DEFAULT_MODULES);
    let mut modules: Vec<Box<dyn Module>> = Vec::new();
    let mut problems = Vec::new();
    for id in list.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()) {
        if modules.iter().any(|m| m.id() == id) {
            continue;
        }
        match make(id, cfg) {
            Some(m) => modules.push(m),
            None => problems.push(format!("unknown module '{id}' in [MILLGUI]MODULES")),
        }
    }
    (modules, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlisted_modules_do_not_exist() {
        let d = std::env::temp_dir().join(format!("millgui-mod-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("m.ini"), "[MILLGUI]\nMODULES = run jog bogus\n").unwrap();
        let cfg = Config::load(&d.join("m.ini")).unwrap();
        let (mods, problems) = build(&cfg);
        let ids: Vec<_> = mods.iter().map(|m| m.id()).collect();
        assert_eq!(ids, ["run", "manual"]);
        assert_eq!(problems.len(), 1);
        std::fs::remove_dir_all(d).ok();
    }
}
