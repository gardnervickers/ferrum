//! SETUP: the configuration checker's rows on nine pages, the ferrum.toml
//! settings each page asks for, live tests of the probe and tool setter
//! inputs, the HAL wiring with the machine's own pins, and Ferrum's
//! version and updates (updater.rs).
//!
//! A setting is written to ferrum.toml at once (comments kept). ferrum
//! reads its settings when it starts, as LinuxCNC reads the INI, so a
//! saved change shows as waiting until LinuxCNC (and with it ferrum) next
//! starts. RESTART LINUXCNC (only with the machine off and idle) does that
//! from the screen, and first adds any INI lines queued with ADD TO INI.
//! With no ferrum.toml, SETUP opens first; FINISH writes the file.

use crate::checks::{Check, Fix, Group, Level};
use crate::config::Config;
use crate::halmap::HalItem;
use crate::machine_cfg::{self, MachineCfg, Park, ToolChangeStyle};
use crate::machine_info::{self, MachineInfo};
use crate::modules::SetterHal;
use crate::updater::{self, Updater};
use crate::{Actions, Live, MainWindow, SetupRow, SetupStep, StatusRow};
use machine::Status;
use slint::{ComponentHandle, Model};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Machine,
    Spindle,
    ToolChange,
    Probe,
    ToolSetter,
    Screens,
    Hal,
    Display,
    Software,
}

pub const STEPS: [Step; 9] =
    [Step::Machine, Step::Spindle, Step::ToolChange, Step::Probe, Step::ToolSetter, Step::Screens, Step::Hal, Step::Display, Step::Software];

impl Step {
    fn label(self) -> &'static str {
        match self {
            Step::Machine => "MACHINE",
            Step::Spindle => "SPINDLE",
            Step::ToolChange => "TOOL CHANGE",
            Step::Probe => "PROBE",
            Step::ToolSetter => "TOOL SETTER",
            Step::Screens => "SCREENS",
            Step::Hal => "HAL WIRING",
            Step::Display => "DISPLAY",
            Step::Software => "SOFTWARE",
        }
    }

    fn intro(self) -> &'static str {
        match self {
            Step::Machine => "What ferrum read from the INI. Check the units and travel, and say where retracts and PARK go.",
            Step::Spindle => "Every speed ferrum sends is checked against the top speed. CCW shows only when the spindle reverses.",
            Step::ToolChange => "Who answers M6. ferrum works it out from HAL; set it here only when that is wrong.",
            Step::Probe => "PROBE is on only with a probe. Touch the probe to test its input before the first probe move.",
            Step::ToolSetter => "Touch-off goes to the G30 position and probes down. Jog over the setter on MANUAL, then set the position here.",
            Step::Screens => "Which screens are in the nav bar, in what order, and which machine modules run. SETUP is always last.",
            Step::Hal => "What ferrum reads from the machine's HAL and drives in it. Tap a row to pick a pin or signal; the light shows its value now.",
            Step::Display => "The quick buttons, the table drawn in the backplot, how the screen draws, and the browser screen.",
            Step::Software => "The version in use and its updates. A new version goes into use when LinuxCNC next starts; the one before stays installed.",
        }
    }

    /// The step a checker row belongs to.
    pub fn of(g: Group) -> Step {
        match g {
            Group::Config | Group::Machine | Group::Files => Step::Machine,
            Group::Spindle => Step::Spindle,
            Group::ToolChange => Step::ToolChange,
            Group::Probe => Step::Probe,
            Group::ToolSetter => Step::ToolSetter,
            Group::Job => Step::Screens,
            Group::Hal => Step::Hal,
            Group::Display => Step::Display,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Number { min: f64, max: f64, whole: bool },
    Bool,
    /// (value written, label shown); the value "" removes the key.
    Choice(&'static [(&'static str, &'static str)]),
    /// Words on the keyboard; empty removes the key.
    Text,
}

#[derive(Clone, Debug)]
struct Setting {
    key: &'static str,
    step: Step,
    title: &'static str,
    detail: &'static str,
    kind: Kind,
    /// A caption above it, starting a group of rows.
    head: &'static str,
}

const fn set(key: &'static str, step: Step, title: &'static str, detail: &'static str, kind: Kind) -> Setting {
    Setting { key, step, title, detail, kind, head: "" }
}

impl Setting {
    fn head(mut self, h: &'static str) -> Self {
        self.head = h;
        self
    }
}

const PARK: &[(&str, &str)] = &[("machine-zero", "MACHINE X0 Y0"), ("g28", "G28 POSITION")];
const STYLE: &[(&str, &str)] =
    &[("", "DETECT FROM HAL"), ("external", "THE MACHINE (HAL)"), ("dialog", "FERRUM'S DIALOG"), ("drawbar", "DIALOG + DRAWBAR")];
const RENDERER: &[(&str, &str)] = &[("", "AUTO"), ("gpu", "GPU"), ("software", "SOFTWARE")];
const LENGTH: Kind = Kind::Number { min: -100_000.0, max: 100_000.0, whole: false };
const SIZE: Kind = Kind::Number { min: 0.0, max: 100_000.0, whole: false };

/// The settings SETUP asks for, with this machine's ranges.
fn settings(cfg: &Config) -> Vec<Setting> {
    let zmin = cfg.get_f64("AXIS_Z", "MIN_LIMIT").unwrap_or(f64::MIN);
    let zmax = cfg.get_f64("AXIS_Z", "MAX_LIMIT").unwrap_or(f64::MAX);
    let tool = Kind::Number { min: 1.0, max: 99_999.0, whole: true };
    vec![
        set("machine.z_top", Step::Machine, "Retract height (machine Z)", "Every retract and PARK goes up to it first.", Kind::Number { min: zmin, max: zmax, whole: false }),
        set("machine.park", Step::Machine, "PARK goes to", "After Z is up.", Kind::Choice(PARK)),
        set(
            "machine.screen_watchdog",
            Step::Machine,
            "Screen watchdog (recommended)",
            "E-stops the machine when this screen stops drawing for a second: a frozen or crashed screen cannot leave the machine running.",
            Kind::Bool,
        ),
        set("spindle.max_rpm", Step::Spindle, "Top speed (rpm)", "Faster speeds are refused.", Kind::Number { min: 1.0, max: 100_000.0, whole: true }),
        set("spindle.reverse", Step::Spindle, "The spindle runs in reverse (M4)", "Shows CCW on MANUAL.", Kind::Bool),
        set("toolchange.style", Step::ToolChange, "M6 is answered by", "", Kind::Choice(STYLE)),
        set("toolchange.scratch_tools.0", Step::ToolChange, "First scratch tool number", "TOOLS › NEW SCRATCH numbers a one-off tool from this range; changing to one asks for a touch-off.", tool),
        set("toolchange.scratch_tools.1", Step::ToolChange, "Last scratch tool number", "", tool),
        set("probe.enabled", Step::Probe, "This machine has a probe", "Without one, PROBE leaves the nav bar.", Kind::Bool),
        set("probe.tool", Step::Probe, "Probe tool number", "The spindle cannot start with it loaded.", tool),
        set("toolsetter.enabled", Step::ToolSetter, "This machine has a tool setter", "", Kind::Bool),
        set("buttons.home_all", Step::Display, "HOME ALL button", "Under the overrides, until the machine is homed.", Kind::Bool).head("QUICK BUTTONS"),
        set("buttons.mist", Step::Display, "MIST button", "Under the overrides.", Kind::Bool),
        set("buttons.flood", Step::Display, "FLOOD button", "Under the overrides.", Kind::Bool),
        set("buttons.drawbar", Step::Display, "RELEASE TOOL button", "Under the overrides, with the power drawbar.", Kind::Bool),
        set("model.show", Step::Display, "Draw the table in the backplot", "The table, its slots and the spindle, to scale.", Kind::Bool).head("BACKPLOT TABLE"),
        set("model.table.0", Step::Display, "Table length (X)", "", SIZE),
        set("model.table.1", Step::Display, "Table depth (Y)", "", SIZE),
        set("model.table.2", Step::Display, "Table thickness", "", SIZE),
        set("model.table_center.0", Step::Display, "Table middle: machine X", "Machine X with the spindle over the middle of the table.", LENGTH),
        set("model.table_center.1", Step::Display, "Table middle: machine Y", "", LENGTH),
        set("model.slots", Step::Display, "T-slots", "", Kind::Number { min: 0.0, max: 20.0, whole: true }),
        set("model.nose_above_table", Step::Display, "Spindle nose above the table at machine Z 0", "0 hides the table.", SIZE),
        set("display.renderer", Step::Display, "Drawing", "AUTO uses the GPU when the driver is a real one.", Kind::Choice(RENDERER)).head("SCREEN"),
        set("display.mdi_library", Step::Display, "MDI library file", "The MDI screen's saved commands. Relative to the INI's folder; empty: ferrum/mdi.toml.", Kind::Text),
        set("remote.port", Step::Display, "Browser screen port", "0 turns it off. Anyone who can reach the port can run the machine.", Kind::Number { min: 0.0, max: 65_535.0, whole: true }),
    ]
    .into_iter()
    .chain(crate::probe_defaults::DEFS.iter().map(|d| {
        let inch = machine_info::get().units.inch();
        Setting {
            key: d.key,
            step: if d.setter() { Step::ToolSetter } else { Step::Probe },
            title: d.title,
            detail: match (d.unit, inch) {
                (crate::probe_defaults::Unit::Feed, false) => "mm/min",
                (crate::probe_defaults::Unit::Feed, true) => "in/min",
                (_, false) => "mm",
                (_, true) => "in",
            },
            kind: Kind::Number { min: 0.0, max: 100_000.0, whole: false },
            head: "",
        }
    }))
    .collect()
}

/// Settings whose value is a length in machine units.
fn is_length(key: &str) -> bool {
    key == "machine.z_top" || key.starts_with("model.table") || key == "model.nose_above_table"
}

/// `table.key.<i>`: one entry of an array setting.
fn indexed(key: &str) -> Option<(&str, usize)> {
    let (base, i) = key.rsplit_once('.')?;
    Some((base, i.parse().ok()?))
}

/// The screens SETUP › SCREENS can turn on, in the default order.
const SCREENS: &[(&str, &str, &str)] = &[
    ("run", "RUN", "The program, its preview, CYCLE START."),
    ("job", "JOB", "Jobs built from probing, conversational and CAM steps."),
    ("manual", "MANUAL", "Jog, home, MDI, spindle, zero."),
    ("probe", "PROBE", "Probing routines. Needs a probe."),
    ("tools", "TOOLS", "The tool table and touch-off."),
    ("offsets", "OFFSETS", "Work offsets."),
    ("conv", "CONV", "Conversational facing, pockets, holes and threads."),
    ("files", "FILES", "Open a program."),
    ("util", "UTIL", "Warm-up, the tool setter position, machine status."),
];

/// Machine modules: no screen of their own. (id, title, what, needs a HAL file).
const HARDWARE: &[(&str, &str, &str, bool)] = &[
    ("toolchange", "Tool change dialog", "Answers M6 with ferrum's dialog. Added by itself when nothing in HAL answers M6.", false),
    ("drawbar", "Power drawbar", "The tool change dialog with a power drawbar and RELEASE TOOL.", true),
    ("drives", "Drive status", "Each axis's servo drive ready and fault, in the status list.", true),
    ("interlock", "Interlock reasons", "Says why motion is held. Wired on HAL WIRING.", false),
    ("load", "Cutting load", "Colours the backplot trail by spindle load. Wired on HAL WIRING.", false),
    ("pendant", "Pendant", "A pendant's start button and mode requests.", true),
];

/// One `[hal]` name as HAL WIRING lists it.
#[derive(Clone, Copy, Debug)]
struct HalName {
    module: &'static str,
    name: &'static str,
    pin: &'static str,
    ty: &'static str,
    input: bool,
    auto: Option<&'static str>,
    title: &'static str,
}

/// The screen watchdog's trip: no ferrum pin of its own.
const WATCHDOG_NAME: HalName =
    HalName { module: "watchdog", name: crate::halpins::WATCHDOG_TRIP, pin: "watchdog.ok", ty: "bit", input: false, auto: None, title: "Frozen screen trips E-stop" };

fn hal_name(d: &crate::halpins::Def, module: &'static str) -> HalName {
    HalName { module, name: d.name.unwrap_or(""), pin: d.pin, ty: d.ty.as_str(), input: d.input, auto: d.auto, title: d.title }
}

/// Every `[hal]` name, with the first module that uses it.
fn all_hal_names() -> Vec<HalName> {
    let mut v: Vec<HalName> = Vec::new();
    for d in crate::halpins::ALL.iter().filter(|d| d.name.is_some()) {
        if !v.iter().any(|h| Some(h.name) == d.name) {
            v.push(hal_name(d, d.modules[0]));
        }
    }
    v.push(WATCHDOG_NAME);
    v
}

fn hal_meta(name: &str) -> (&'static str, &'static str) {
    all_hal_names().into_iter().find(|h| h.name == name).map_or(("", "bit"), |h| (h.title, h.ty))
}

fn yes(b: bool) -> String {
    if b { "YES" } else { "NO" }.into()
}

fn style_label(s: ToolChangeStyle) -> &'static str {
    match s {
        ToolChangeStyle::External => "THE MACHINE (HAL)",
        ToolChangeStyle::Dialog => "FERRUM'S DIALOG",
        ToolChangeStyle::Drawbar => "DIALOG + DRAWBAR",
    }
}

#[cfg(test)]
use machine_cfg::edited;

/// What the open picker is choosing.
enum Pick {
    Setting(&'static str),
    /// A HAL name: its first two items, then these targets.
    Hal(String, Vec<String>),
    Channel,
}

/// The HAL list, refreshed on a thread while HAL WIRING shows.
#[derive(Default)]
struct HalFeed {
    want: AtomicBool,
    gen: AtomicU64,
    items: Mutex<Option<Vec<HalItem>>>,
}

pub struct Setup {
    ui: slint::Weak<MainWindow>,
    cfg: Config,
    checks: Vec<Check>,
    settings: Vec<Setting>,
    modules: Vec<String>,
    /// Where settings are written, and the older file a new one starts from.
    path: PathBuf,
    seed: Option<PathBuf>,
    step: usize,
    /// Saved changes, shown until LinuxCNC next starts.
    pending: BTreeMap<String, String>,
    /// The picker open for this page.
    picking: Option<Pick>,
    setter: Option<Rc<SetterHal>>,
    /// Who answers M6 as HAL has it, whatever [toolchange]style says.
    detected: ToolChangeStyle,
    /// Live test inputs last shown: probe, setter, G30 text.
    live: (Option<bool>, Option<bool>, String),
    /// The probe module, which holds the probe's and setter's values.
    probe: Option<Rc<RefCell<crate::probe::Probe>>>,
    /// The module list as written, and as ferrum runs now.
    order: Vec<String>,
    order_running: Vec<String>,
    /// `[modules]enabled` is (or will be) in the file.
    order_written: bool,
    hal: Arc<HalFeed>,
    hal_seen: u64,
    updater: Updater,
    upd: Option<Result<updater::Status, String>>,
    upd_error: Option<String>,
    upd_read: Option<Instant>,
    /// INI fixes to write when RESTART LINUXCNC stops LinuxCNC (check ids).
    ini_queue: BTreeSet<&'static str>,
    can_restart: bool,
    /// Picks up what the HAL and updater threads read, without waiting
    /// for a status change.
    timer: Option<slint::Timer>,
}

impl Setup {
    fn mc(&self) -> &'static MachineCfg {
        machine_cfg::get()
    }

    fn mi(&self) -> &'static MachineInfo {
        machine_info::get()
    }

    fn has(&self, m: &str) -> bool {
        self.modules.iter().any(|x| x == m)
    }

    fn setter_on(&self) -> bool {
        self.mc().toolsetter.enabled.unwrap_or(self.has("util"))
    }

    fn hal_items(&self) -> Option<Vec<HalItem>> {
        self.hal.items.lock().ok().and_then(|g| g.clone())
    }

    /// The setting's value as ferrum runs now.
    fn running(&self, key: &str) -> String {
        let (mc, mi, cfg) = (self.mc(), self.mi(), &self.cfg);
        let ui = self.ui.upgrade();
        let live = ui.as_ref().map(|ui| ui.global::<Live>());
        let num = |v: f64| if is_length(key) { mi.units.fmt(v) } else { fmt_num(v) };
        match key {
            "machine.z_top" => mi.units.fmt(mi.z_top),
            "machine.park" => match mc.machine.park {
                Park::MachineZero => PARK[0].1.into(),
                Park::G28 => PARK[1].1.into(),
                Park::Xy(x, y) => format!("X{} Y{}", mi.units.fmt(x), mi.units.fmt(y)),
            },
            "spindle.max_rpm" => mi.max_rpm.map_or("NOT SET".into(), |v| format!("{v:.0}")),
            "spindle.reverse" => yes(mi.reverse),
            "machine.screen_watchdog" => yes(mc.screen_watchdog()),
            "buttons.mist" => yes(live.as_ref().map_or(mc.buttons.mist, |l| l.get_quick_mist())),
            "buttons.flood" => yes(live.as_ref().map_or(mc.buttons.flood, |l| l.get_quick_flood())),
            "buttons.drawbar" => yes(live.as_ref().map_or(mc.buttons.drawbar, |l| l.get_quick_drawbar())),
            "buttons.home_all" => yes(live.as_ref().map_or(mc.buttons.home_all, |l| l.get_quick_home())),
            "toolchange.style" => match mc.toolchange.style {
                Some(s) => style_label(s).into(),
                None => format!("DETECTED: {}", style_label(self.detected)),
            },
            "toolchange.scratch_tools.0" => mc.toolchange.scratch_tools.map_or("NOT SET".into(), |t| t[0].to_string()),
            "toolchange.scratch_tools.1" => mc.toolchange.scratch_tools.map_or("NOT SET".into(), |t| t[1].to_string()),
            "probe.enabled" => yes(self.has("probe")),
            "probe.tool" => mc.probe_tool(cfg).to_string(),
            "toolsetter.enabled" => yes(self.setter_on()),
            "remote.port" => match mc.remote_port(cfg) {
                Some(Ok(p)) => p.to_string(),
                _ => "0".into(),
            },
            "model.show" => yes(mc.model.show),
            "model.table.0" => num(mc.model.table[0]),
            "model.table.1" => num(mc.model.table[1]),
            "model.table.2" => num(mc.model.table[2]),
            "model.table_center.0" => num(mc.model.table_center[0]),
            "model.table_center.1" => num(mc.model.table_center[1]),
            "model.slots" => mc.model.slots.to_string(),
            "model.nose_above_table" => num(mc.model.nose_above_table),
            "display.renderer" => match mc.renderer(cfg).as_deref() {
                Some("gpu") => "GPU".into(),
                Some("software") => "SOFTWARE".into(),
                _ => "AUTO".into(),
            },
            "display.mdi_library" => mc.mdi_library(cfg).unwrap_or_default(),
            k if k.starts_with("hal.") => self.hal_target_running(&k[4..]),
            k => match self.stored(k) {
                Some((Some(v), _)) => fmt_num(v),
                Some((None, _)) => "NOT SET".into(),
                None => String::new(),
            },
        }
    }

    fn value(&self, key: &str) -> String {
        self.pending.get(key).cloned().unwrap_or_else(|| self.running(key))
    }

    fn setting(&self, key: &str) -> Option<&Setting> {
        self.settings.iter().find(|s| s.key == key)
    }

    /// Write one key; the shown value waits for the next start.
    fn save(&mut self, key: &str, value: Option<toml_edit::Item>, shown: String) -> Result<(), String> {
        let toml_key = indexed(key).map_or(key, |(base, _)| base);
        machine_cfg::write_key(&self.path, self.seed.as_deref(), toml_key, value)?;
        self.note(key, shown);
        Ok(())
    }

    /// Mark `key` as waiting for a restart, unless it is back to what runs.
    fn note(&mut self, key: &str, shown: String) {
        if shown == self.running(key) {
            self.pending.remove(key);
        } else {
            self.pending.insert(key.to_string(), shown);
        }
    }

    /// A probe or setter value as the probe module uses it now.
    fn stored(&self, key: &str) -> Option<(Option<f64>, crate::probe_defaults::Source)> {
        let d = crate::probe_defaults::by_key(key)?;
        Some(match &self.probe {
            Some(p) => p.borrow().default_of(d.n),
            None => crate::probe_defaults::resolve(self.mc(), &crate::probe_defaults::param_file(&self.cfg), self.mi().units.inch())[&d.n],
        })
    }

    fn set_number(&mut self, key: &'static str, v: f64) -> Result<(), String> {
        let Some(s) = self.setting(key) else { return Err("not a setting".into()) };
        let Kind::Number { min, max, whole } = s.kind else { return Err("not a number".into()) };
        if !(min..=max).contains(&v) || (whole && v.fract() != 0.0) {
            let unit = if is_length(key) { format!(" {}", self.mi().units.name()) } else { String::new() };
            let range = if min > f64::MIN && max < f64::MAX { format!(": {} to {}{unit}", fmt_num(min), fmt_num(max)) } else { String::new() };
            return Err(format!("Out of range{range}{}", if whole { ", whole numbers" } else { "" }));
        }
        // A probe or setter value applies from the next run on.
        if let Some(d) = crate::probe_defaults::by_key(key) {
            crate::probe_defaults::check_stopping_feed(d.n, v, self.mi())?;
            machine_cfg::write_key(&self.path, self.seed.as_deref(), key, Some(toml_edit::value(v)))?;
            if let Some(p) = &self.probe {
                p.borrow_mut().adopt(d.n, v);
            }
            return Ok(());
        }
        let shown = if is_length(key) { self.mi().units.fmt(v) } else { fmt_num(v) };
        if let Some((base, i)) = indexed(key) {
            return self.set_index(base, i, v, whole, shown);
        }
        let item = if whole { toml_edit::value(v as i64) } else { toml_edit::value(v) };
        self.save(key, Some(item), shown)
    }

    /// One entry of an array setting; the others keep their shown values
    /// (an unset scratch range starts as one tool).
    fn set_index(&mut self, base: &str, i: usize, v: f64, whole: bool, shown: String) -> Result<(), String> {
        let n = if base == "toolchange.scratch_tools" { 2 } else if base == "model.table" { 3 } else { 2 };
        let mut vals: Vec<f64> = (0..n).map(|j| self.value(&format!("{base}.{j}")).split_whitespace().next().unwrap_or("").parse().unwrap_or(f64::NAN)).collect();
        vals[i] = v;
        for j in 0..n {
            if vals[j].is_nan() {
                vals[j] = v;
            }
        }
        if base == "toolchange.scratch_tools" && vals[0] > vals[1] {
            // Keep the range in order: the other end follows.
            let other = 1 - i;
            vals[other] = v;
        }
        let mut arr = toml_edit::Array::new();
        for x in &vals {
            if whole {
                arr.push(*x as i64);
            } else {
                arr.push(*x);
            }
        }
        machine_cfg::write_key(&self.path, self.seed.as_deref(), base, Some(toml_edit::value(arr)))?;
        let fmt = |x: f64| if is_length(base) { self.mi().units.fmt(x) } else { fmt_num(x) };
        let shown_all: Vec<String> = vals.iter().map(|x| fmt(*x)).collect();
        for (j, s) in shown_all.into_iter().enumerate() {
            let k = format!("{base}.{j}");
            if j == i {
                self.note(&k, shown.clone());
            } else {
                self.note(&k, s);
            }
        }
        Ok(())
    }

    fn toggle(&mut self, key: &'static str) -> Result<(), String> {
        let now = self.value(key) == "YES";
        // The quick buttons change at once.
        if let (Some(b), Some(ui)) = (key.strip_prefix("buttons."), self.ui.upgrade()) {
            machine_cfg::write_key(&self.path, self.seed.as_deref(), key, Some(toml_edit::value(!now)))?;
            let live = ui.global::<Live>();
            match b {
                "mist" => live.set_quick_mist(!now),
                "flood" => live.set_quick_flood(!now),
                "drawbar" => live.set_quick_drawbar(!now),
                "home_all" => live.set_quick_home(!now),
                _ => {}
            }
            return Ok(());
        }
        self.save(key, Some(toml_edit::value(!now)), yes(!now))?;
        // The probe is a screen too: keep SCREENS in step.
        if key == "probe.enabled" {
            self.order.retain(|m| m != "probe");
            if !now {
                let at = self.order.iter().position(|m| m == "manual").map_or(self.order.len(), |i| i + 1);
                self.order.insert(at, "probe".into());
            }
            if self.order_written {
                self.write_order()?;
            }
        }
        Ok(())
    }

    fn choose(&mut self, key: &'static str, i: usize) -> Result<(), String> {
        let Some(Kind::Choice(items)) = self.setting(key).map(|s| s.kind) else { return Err("not a choice".into()) };
        let (v, label) = items.get(i).ok_or("no such choice")?;
        let shown = if key == "toolchange.style" && v.is_empty() { format!("DETECTED: {}", style_label(self.detected)) } else { label.to_string() };
        self.save(key, if v.is_empty() { None } else { Some(toml_edit::value(*v)) }, shown)
    }

    fn set_text(&mut self, key: &str, v: &str) -> Result<(), String> {
        if let Some(name) = key.strip_prefix("hal.") {
            if v.chars().any(char::is_whitespace) {
                return Err("A HAL name has no spaces".into());
            }
            if let Some(items) = self.hal_items() {
                if !v.is_empty() && !items.iter().any(|i| i.name == v) {
                    return Err(format!("HAL has no pin or signal named {v}"));
                }
            }
            return self.set_hal(name, (!v.is_empty()).then_some(v));
        }
        self.save(key, (!v.is_empty()).then(|| toml_edit::value(v)), v.to_string())
    }

    // SCREENS

    /// The written module list (nav order).
    fn write_order(&mut self) -> Result<(), String> {
        machine_cfg::write_key(&self.path, self.seed.as_deref(), "modules.enabled", Some(toml_edit::value(self.order.join(" "))))?;
        self.order_written = true;
        if self.order == self.order_running {
            self.pending.remove("modules.enabled");
        } else {
            self.pending.insert("modules.enabled".into(), self.order.join(" "));
        }
        Ok(())
    }

    fn toggle_module(&mut self, id: &str) -> Result<(), String> {
        if let Some(i) = self.order.iter().position(|m| m == id) {
            self.order.remove(i);
        } else {
            // A screen goes after the last screen; a module at the end.
            let screen = SCREENS.iter().any(|s| s.0 == id);
            let at = if screen { self.order.iter().rposition(|m| SCREENS.iter().any(|s| s.0 == m)).map_or(0, |i| i + 1) } else { self.order.len() };
            self.order.insert(at, id.to_string());
            // A probe switched off in [probe] comes back with its screen.
            if id == "probe" && self.value("probe.enabled") == "NO" {
                self.save("probe.enabled", None, "YES".into())?;
            }
        }
        if id == "probe" && !self.order.iter().any(|m| m == "probe") && self.value("probe.enabled") == "YES" {
            self.note("probe.enabled", "NO".into());
        }
        self.write_order()
    }

    /// Move a screen one place up (-1) or down (+1) among the screens.
    fn move_screen(&mut self, id: &str, by: i32) -> Result<(), String> {
        let screens: Vec<usize> = self.order.iter().enumerate().filter(|(_, m)| SCREENS.iter().any(|s| s.0 == m.as_str())).map(|(i, _)| i).collect();
        let Some(k) = screens.iter().position(|&i| self.order[i] == id) else { return Ok(()) };
        let j = k as i32 + by;
        if j < 0 || j as usize >= screens.len() {
            return Ok(());
        }
        self.order.swap(screens[k], screens[j as usize]);
        self.write_order()
    }

    fn reset_order(&mut self) -> Result<(), String> {
        machine_cfg::write_key(&self.path, self.seed.as_deref(), "modules.enabled", None)?;
        self.order_written = false;
        self.order = initial_order(self.mc(), &self.cfg, false);
        if self.order == self.order_running {
            self.pending.remove("modules.enabled");
        } else {
            self.pending.insert("modules.enabled".into(), self.order.join(" "));
        }
        Ok(())
    }

    // HAL WIRING

    /// The module whose pin a HAL name connects.
    fn hal_module(&self, name: &str) -> Option<&'static str> {
        all_hal_names().into_iter().find(|h| h.name == name).map(|h| h.module)
    }

    fn hal_link(&self, name: &str) -> Option<HalName> {
        all_hal_names().into_iter().find(|h| h.name == name)
    }

    /// The old per-module HAL file runs: there is one and ferrum.toml has
    /// no `[hal]` table.
    fn has_hal_file(&self, module: &str) -> bool {
        !self.mc().has_hal_table() && self.cfg.dir().join(format!("ferrum/hal/{module}.hal")).is_file()
    }

    /// What the name connects to as ferrum runs: `[hal]`, the module's HAL
    /// file, the default, or nothing.
    fn hal_target_running(&self, name: &str) -> String {
        match self.mc().hal.get(name) {
            Some(t) => t.text(),
            None => self.hal_fallback(name),
        }
    }

    /// What a name connects to with no `[hal]` entry.
    fn hal_fallback(&self, name: &str) -> String {
        let module = self.hal_module(name).unwrap_or("");
        if self.has_hal_file(module) {
            return format!("ferrum/hal/{module}.hal");
        }
        match self.hal_link(name).and_then(|l| l.auto) {
            Some(a) => format!("{a} (default)"),
            None if name == crate::halpins::WATCHDOG_TRIP => "E-STOP INPUT (default)".into(),
            None => "NOT CONNECTED".into(),
        }
    }

    fn set_hal(&mut self, name: &str, target: Option<&str>) -> Result<(), String> {
        let key = format!("hal.{name}");
        // A name picked for an inverted entry stays inverted.
        let inverted = self.mc().hal.get(name).is_some_and(|t| t.invert);
        let picked = target.map(|t| if inverted && !t.starts_with('!') { format!("!{t}") } else { t.to_string() });
        let target = picked.as_deref();
        machine_cfg::write_key(&self.path, self.seed.as_deref(), &key, target.map(toml_edit::value))?;
        let shown = match target {
            Some(t) => t.to_string(),
            None => self.hal_fallback(name),
        };
        self.note(&key, shown);
        Ok(())
    }

    /// The names HAL WIRING lists: those of the modules that run (the
    /// machine signals always), and the screen watchdog's.
    fn hal_names(&self) -> Vec<(&'static str, HalName)> {
        let mut v: Vec<(&'static str, HalName)> = Vec::new();
        for d in crate::halpins::ALL.iter().filter(|d| d.name.is_some()) {
            let Some(m) = d.modules.iter().find(|m| crate::halpins::ALWAYS_ON.contains(m) || self.has(m)) else { continue };
            if d.axis.is_some_and(|a| !self.cfg.coordinates().contains(a)) {
                continue;
            }
            if !v.iter().any(|(_, x)| Some(x.name) == d.name) {
                v.push((m, hal_name(d, m)));
            }
        }
        // Grouped by module, in the order modules first appear.
        let order: Vec<&str> = v.iter().map(|(m, _)| *m).fold(Vec::new(), |mut o, m| {
            if !o.contains(&m) {
                o.push(m);
            }
            o
        });
        v.sort_by_key(|(m, _)| order.iter().position(|x| x == m));
        v.push(("watchdog", WATCHDOG_NAME));
        v
    }

    fn open_hal_picker(&mut self, ui: &MainWindow, name: &str) {
        let Some(link) = self.hal_names().into_iter().find(|(_, l)| l.name == name).map(|(_, l)| l) else { return };
        let (title, typ) = hal_meta(name);
        let first = match link.auto {
            Some(a) => format!("DEFAULT: {a}"),
            None if name == crate::halpins::WATCHDOG_TRIP => "DEFAULT: GATE THE E-STOP INPUT".into(),
            None => "NOT CONNECTED".into(),
        };
        let targets: Vec<String> = self
            .hal_items()
            .map(|all| crate::halmap::candidates(&all, typ, link.input).iter().take(400).map(|i| i.name.clone()).collect())
            .unwrap_or_default();
        let now = self.value(&format!("hal.{name}"));
        let current = targets.iter().position(|t| *t == now).map(|i| i + 2).or(if self.mc().hal.contains_key(name) || self.pending.contains_key(&format!("hal.{name}")) { None } else { Some(0) });
        let mut items = vec![first, "TYPE A NAME…".into()];
        items.extend(targets.iter().cloned());
        self.picking = Some(Pick::Hal(name.to_string(), targets));
        crate::picker::open(ui, "setup", &title.to_uppercase(), items, current);
    }

    // SOFTWARE

    fn refresh_updates(&mut self, force: bool) {
        if !self.updater.present() || self.updater.busy.is_some() {
            return;
        }
        if force || self.upd_read.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
            self.updater.start(updater::Op::Status, Vec::new());
        }
    }

    fn update_action(&mut self, action: &str) {
        let s = self.upd.as_ref().and_then(|r| r.as_ref().ok());
        if let Some((op, args)) = updater::command(action, s) {
            self.upd_error = None;
            self.updater.start(op, args);
        }
    }

    fn staged(&self) -> Option<String> {
        self.upd.as_ref().and_then(|r| r.as_ref().ok()).and_then(|s| s.staged.clone())
    }

    /// The one-time conversion of the INI's postgui files (postgui.rs).
    fn convert_postgui(&mut self) -> Result<(), String> {
        let r = crate::postgui::convert(&self.cfg, &self.path, self.seed.as_deref())?;
        self.checks.retain(|c| c.id != "hal.postgui");
        self.pending.insert("display.postgui".into(), r.summary());
        if let Some(ui) = self.ui.upgrade() {
            let live = ui.global::<Live>();
            live.set_message(format!("{}. Applies when LinuxCNC next starts.", r.summary()).into());
            live.set_message_error(false);
        }
        Ok(())
    }

    /// FINISH: make sure ferrum.toml exists, so SETUP stops opening first.
    fn finish(&mut self) -> Result<(), String> {
        if !self.path.exists() {
            machine_cfg::write_file(&self.path, &machine_cfg::new_file_text(self.seed.as_deref()))?;
        }
        Ok(())
    }

    fn step(&self) -> Step {
        STEPS[self.step]
    }

    /// The INI fixes queued for the restart.
    pub fn queued_ini(&self) -> Vec<Check> {
        self.checks.iter().filter(|c| self.ini_queue.contains(c.id)).cloned().collect()
    }

    fn setting_row(&self, s: &Setting) -> SetupRow {
        let level = |l: &Level| match l {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Attention => "warn",
            Level::Blocks(_) => "bad",
        };
        let check = self.checks.iter().find(|c| c.fix == Fix::Setting(key_static(s.key)));
        let kind = match s.kind {
            Kind::Number { .. } => "number",
            Kind::Bool => "bool",
            Kind::Choice(_) => "choice",
            Kind::Text => "text",
        };
        // A setting that needs attention says why; then what was found.
        let mut detail = match check {
            Some(c) if matches!(c.level, Level::Attention | Level::Blocks(_)) => format!("{}.", c.title),
            _ => s.detail.to_string(),
        };
        if let Some(c) = check.filter(|c| !c.detail.is_empty()) {
            detail = format!("{detail} {}", c.detail).trim().to_string();
        }
        if is_length(s.key) {
            detail = format!("{detail} {}", self.mi().units.name()).trim().to_string();
        }
        let pending = self.pending.contains_key(s.key);
        let value = self.value(s.key);
        SetupRow {
            kind: kind.into(),
            key: s.key.into(),
            level: if pending { "warn" } else { check.map_or("", |c| level(&c.level)) }.into(),
            title: s.title.into(),
            detail: detail.into(),
            value: if kind == "text" && value.is_empty() { "DEFAULT".into() } else { value.into() },
            note: if pending { "Saved. Applies when LinuxCNC next starts." } else { "" }.into(),
            ..Default::default()
        }
    }

    fn head(text: &str) -> SetupRow {
        SetupRow { kind: "head".into(), title: text.into(), ..Default::default() }
    }

    /// The rows of the current step: its settings (titled by the check
    /// about them, if any), its live tests, then its checks, worst first.
    fn rows(&self) -> Vec<SetupRow> {
        let step = self.step();
        let mut rows = Vec::new();
        for s in self.settings.iter().filter(|s| s.step == step) {
            // The probe tool only matters with a probe.
            if s.key == "probe.tool" && !self.has("probe") {
                continue;
            }
            // RELEASE TOOL only exists with the drawbar.
            if s.key == "buttons.drawbar" && !self.has("drawbar") {
                continue;
            }
            // The table's size only matters when it is drawn.
            if s.key.starts_with("model.") && s.key != "model.show" && self.value("model.show") != "YES" {
                continue;
            }
            // The stored values go last (below).
            if crate::probe_defaults::by_key(s.key).is_some() {
                continue;
            }
            if !s.head.is_empty() {
                rows.push(Self::head(s.head));
            }
            rows.push(self.setting_row(s));
        }
        // On the long lists, what needs attention comes first.
        if matches!(step, Step::Screens | Step::Hal) {
            self.check_rows(step, &mut rows, true);
        }
        match step {
            Step::Screens => self.screen_rows(&mut rows),
            Step::Hal => self.hal_rows(&mut rows),
            Step::Software => self.software_rows(&mut rows),
            _ => {}
        }
        let (probe, setter, g30) = &self.live;
        match step {
            Step::Probe if self.has("probe") => rows.push(SetupRow {
                kind: "live".into(),
                key: "probe-input".into(),
                level: match probe {
                    Some(true) => "ok",
                    Some(false) => "info",
                    None => "",
                }
                .into(),
                title: "Touch the probe".into(),
                detail: "The light turns green while motion.probe-input is on. Green with nothing touching: the input is inverted in HAL.".into(),
                value: match probe {
                    Some(true) => "TRIPPED",
                    Some(false) => "OPEN",
                    None => "NO LINUXCNC",
                }
                .into(),
                ..Default::default()
            }),
            Step::ToolSetter if self.setter_on() => {
                let wired = self.setter.as_ref().is_some_and(|h| h.trip_wired.get());
                rows.push(SetupRow {
                    kind: "live".into(),
                    key: "setter-input".into(),
                    level: match (wired, setter) {
                        (false, _) => "warn",
                        (_, Some(true)) => "ok",
                        _ => "info",
                    }
                    .into(),
                    title: "Press the tool setter".into(),
                    detail: if wired {
                        "The light turns green while the setter's input is on."
                    } else {
                        "ferrum does not see the setter's input. Pick it on HAL WIRING (Tool setter input) to see it here and on UTIL."
                    }
                    .into(),
                    value: match (wired, setter) {
                        (false, _) => "NOT WIRED",
                        (_, Some(true)) => "TRIPPED",
                        _ => "OPEN",
                    }
                    .into(),
                    ..Default::default()
                });
                rows.push(SetupRow {
                    kind: "action".into(),
                    key: "set-g30".into(),
                    level: if g30_set(g30) { "ok" } else { "warn" }.into(),
                    title: format!("Setter position: {g30}").into(),
                    detail: "Jog over the setter on MANUAL with the spindle clear, come back and hold SET POSITION HERE (G30.1).".into(),
                    value: "SET POSITION HERE".into(),
                    hold: true,
                    ..Default::default()
                });
                rows.push(SetupRow {
                    kind: "action".into(),
                    key: "go-g30".into(),
                    title: "Check the position".into(),
                    detail: "Retracts Z, then moves over the setter.".into(),
                    value: "GO TO G30".into(),
                    ..Default::default()
                });
            }
            _ => {}
        }
        self.check_rows(step, &mut rows, false);
        // The probe's or setter's stored values, after its checks.
        let shown = match step {
            Step::Probe => self.has("probe"),
            Step::ToolSetter => self.setter_on(),
            _ => false,
        };
        let mut headed = false;
        for s in self.settings.iter().filter(|s| shown && s.step == step) {
            let Some((v, src)) = self.stored(s.key) else { continue };
            if !headed {
                rows.push(Self::head(if step == Step::Probe { "PROBE VALUES" } else { "TOOL SETTER VALUES" }));
                headed = true;
            }
            use crate::probe_defaults::Source;
            rows.push(SetupRow {
                kind: "number".into(),
                key: s.key.into(),
                level: match src {
                    Source::Seed => "warn",
                    Source::Missing if !s.key.contains("cal_block") => "warn",
                    _ => "",
                }
                .into(),
                title: s.title.into(),
                detail: s.detail.into(),
                value: v.map_or("NOT SET".into(), fmt_num).into(),
                note: match src {
                    Source::Seed => "Starting value: check it.",
                    _ => "",
                }
                .into(),
                ..Default::default()
            });
        }
        rows
    }

    /// The step's checker rows, worst first. `urgent`: only those that need
    /// attention, for the top of a long page; otherwise the rest (or, on a
    /// short page, all).
    fn check_rows(&self, step: Step, rows: &mut Vec<SetupRow>, urgent: bool) {
        let split = matches!(step, Step::Screens | Step::Hal);
        let level = |l: &Level| match l {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Attention => "warn",
            Level::Blocks(_) => "bad",
        };
        let mut checks: Vec<&Check> = self
            .checks
            .iter()
            .filter(|c| Step::of(c.group) == step)
            .filter(|c| !matches!(&c.fix, Fix::Setting(k) if self.setting(k).is_some()))
            // The setter position row above follows SET POSITION HERE.
            .filter(|c| c.id != "toolsetter.g30")
            // The banner says it on a first run.
            .filter(|c| !(c.id == "config.file" && c.level == Level::Info))
            .filter(|c| !split || urgent == matches!(c.level, Level::Attention | Level::Blocks(_)))
            .collect();
        let rank = |c: &Check| match c.level {
            Level::Blocks(_) => 0,
            Level::Attention => 1,
            Level::Info => 2,
            Level::Ok => 3,
        };
        checks.sort_by_key(|c| rank(c));
        if !checks.is_empty() && !rows.is_empty() && !urgent {
            rows.push(Self::head("CHECKS"));
        }
        for c in checks {
            let ini = matches!(c.fix, Fix::IniLines { .. }) && matches!(c.level, Level::Attention | Level::Blocks(_));
            let queued = self.ini_queue.contains(c.id);
            let (code, how) = match &c.fix {
                Fix::IniLines { section, lines } => (
                    format!("[{section}]\n{}", lines.join("\n")),
                    if queued {
                        "Added to the INI when LinuxCNC restarts (the old file is kept as .bak)."
                    } else if self.can_restart {
                        "ADD TO INI writes these lines when LinuxCNC restarts, or add them yourself."
                    } else {
                        "Add to the INI, then restart LinuxCNC."
                    },
                ),
                Fix::Hal(line) => (line.clone(), "Add to the machine's HAL file."),
                _ => (String::new(), ""),
            };
            let detail = [c.detail.as_str(), how].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
            if c.fix == Fix::ConvertPostgui {
                rows.push(SetupRow {
                    kind: "action".into(),
                    key: "convert-postgui".into(),
                    level: level(&c.level).into(),
                    title: c.title.clone().into(),
                    detail: detail.into(),
                    value: "CONVERT".into(),
                    free: true,
                    ..Default::default()
                });
                continue;
            }
            let action = ini && self.can_restart;
            rows.push(SetupRow {
                kind: if action { "action" } else { "check" }.into(),
                key: if action { format!("ini:{}", c.id) } else { c.id.to_string() }.into(),
                level: level(&c.level).into(),
                title: c.title.clone().into(),
                detail: detail.into(),
                value: if queued { "UNDO" } else { "ADD TO INI" }.into(),
                code: code.into(),
                note: if queued { "Waiting for the restart." } else { "" }.into(),
                free: true,
                on: queued,
                ..Default::default()
            });
        }
    }

    fn screen_rows(&self, rows: &mut Vec<SetupRow>) {
        rows.push(Self::head("NAV BAR"));
        let on: Vec<&str> = self.order.iter().map(String::as_str).filter(|m| SCREENS.iter().any(|s| s.0 == *m)).collect();
        let off = SCREENS.iter().map(|s| s.0).filter(|id| !on.contains(id));
        for (k, id) in on.iter().copied().chain(off).enumerate() {
            let (_, label, what) = SCREENS.iter().find(|s| s.0 == id).copied().unwrap_or((id, id, ""));
            let enabled = k < on.len();
            let mut detail = what.to_string();
            if id == "probe" && enabled && self.value("probe.enabled") == "NO" {
                detail = format!("{detail} Off: PROBE says this machine has no probe.");
            }
            rows.push(SetupRow {
                kind: "order".into(),
                key: format!("screen:{id}").into(),
                title: label.into(),
                detail: detail.into(),
                value: yes(enabled).into(),
                state: format!("{}{}", if enabled && k > 0 { "u" } else { "" }, if enabled && k + 1 < on.len() { "d" } else { "" }).into(),
                ..Default::default()
            });
        }
        rows.push(SetupRow { kind: "order".into(), key: "screen:setup".into(), title: "SETUP".into(), detail: "Always there, last.".into(), value: "YES".into(), off: true, ..Default::default() });
        rows.push(Self::head("MACHINE MODULES"));
        for (id, title, what, file) in HARDWARE {
            let enabled = self.order.iter().any(|m| m == id) || (*id == "toolchange" && self.has("toolchange"));
            let auto = *id == "toolchange" && !self.order.iter().any(|m| m == id) && self.has("toolchange");
            let missing = *file && enabled && !self.has_hal_file(id);
            let mut detail = what.to_string();
            if *file {
                detail = format!("{detail} Wired by ferrum/hal/{id}.hal beside the INI.");
            }
            if auto {
                detail = format!("{detail} On now because nothing in HAL answers M6.");
            }
            rows.push(SetupRow {
                kind: "bool".into(),
                key: format!("module:{id}").into(),
                level: if missing { "warn" } else { "" }.into(),
                title: (*title).into(),
                detail: detail.into(),
                value: yes(enabled).into(),
                note: if missing { format!("ferrum/hal/{id}.hal is missing: its pins will not be connected.") } else { String::new() }.into(),
                ..Default::default()
            });
        }
        if self.pending.contains_key("modules.enabled") {
            rows.push(SetupRow { kind: "check".into(), key: "modules-pending".into(), level: "warn".into(), title: "Saved. Applies when LinuxCNC next starts.".into(), ..Default::default() });
        }
        if self.order_written {
            rows.push(SetupRow {
                kind: "action".into(),
                key: "screens-default".into(),
                title: "Back to the default list".into(),
                detail: "Every screen in the usual order, and the tool change dialog when HAL needs it.".into(),
                value: "USE THE DEFAULT".into(),
                free: true,
                ..Default::default()
            });
        }
    }

    fn hal_rows(&self, rows: &mut Vec<SetupRow>) {
        let items = self.hal_items();
        let mut last = "";
        for (module, link) in self.hal_names() {
            if module != last {
                rows.push(Self::head(&module.to_uppercase()));
                last = module;
            }
            let (title, typ) = hal_meta(link.name);
            let key = format!("hal.{}", link.name);
            let target = self.value(&key);
            let name = target.split(" (").next().unwrap_or("");
            let found = items.as_ref().and_then(|all| all.iter().find(|i| i.name == name));
            let named = !target.ends_with("(default)") && target != "NOT CONNECTED" && !target.ends_with(".hal");
            let file = self.has_hal_file(module);
            let dir = if link.input { "ferrum reads it" } else { "ferrum drives it" };
            let mut detail = format!("ferrum.{} · {typ} · {dir}.", link.pin);
            if file {
                detail = format!("{detail} ferrum/hal/{module}.hal wires this module; a name picked here connects on top.");
            }
            let pending = self.pending.contains_key(&key);
            rows.push(SetupRow {
                kind: "hal".into(),
                key: key.clone().into(),
                level: match (items.is_some(), found, named, pending) {
                    (_, _, _, true) => "warn",
                    (true, None, true, _) => "bad",
                    (true, Some(_), _, _) => "ok",
                    _ => "",
                }
                .into(),
                title: title.into(),
                detail: detail.into(),
                value: target.clone().into(),
                state: match (&items, found) {
                    (None, _) => "",
                    (Some(_), Some(i)) => i.value.as_str(),
                    (Some(_), None) if named => "NOT IN HAL",
                    (Some(_), None) => "",
                }
                .to_string()
                .into(),
                note: if pending { "Saved. Connects when LinuxCNC next starts." } else { "" }.into(),
                ..Default::default()
            });
        }
        if items.is_none() {
            rows.insert(
                0,
                SetupRow {
                    kind: "check".into(),
                    key: "hal-off".into(),
                    level: "info".into(),
                    title: "LinuxCNC's HAL is not running".into(),
                    detail: "With LinuxCNC running, each row shows its value and the picker lists the machine's pins and signals. A name can still be typed.".into(),
                    ..Default::default()
                },
            );
        }
    }

    fn software_rows(&self, rows: &mut Vec<SetupRow>) {
        let row = |level: &str, title: String, detail: String| SetupRow { kind: "check".into(), level: level.into(), title: title.into(), detail: detail.into(), ..Default::default() };
        let act = |key: &str, title: String, detail: String, value: &str, hold: bool| SetupRow {
            kind: "action".into(),
            key: key.into(),
            title: title.into(),
            detail: detail.into(),
            value: value.into(),
            hold,
            free: true,
            off: self.updater.busy.is_some(),
            ..Default::default()
        };
        if !self.updater.present() {
            rows.push(row(
                "info",
                "Installed without the updater".into(),
                "This copy of Ferrum was not installed with the install script, so it updates the way it was put here (on mymill: scripts/deploy from the Mac).".into(),
            ));
            return;
        }
        if let Some(op) = self.updater.busy {
            rows.push(row("info", format!("{}…", op.doing()), String::new()));
        }
        if let Some(e) = &self.upd_error {
            rows.push(row("bad", "The updater refused".into(), e.clone()));
        }
        let s = match &self.upd {
            None => return,
            Some(Err(e)) => {
                rows.push(row("bad", "The updater does not answer".into(), e.clone()));
                rows.push(act("upd-refresh", "Ask again".into(), String::new(), "TRY AGAIN", false));
                return;
            }
            Some(Ok(s)) => s,
        };
        if let Some(m) = &s.fell_back {
            rows.push(row("bad", "The new version did not start".into(), format!("{m} The previous version is running.")));
        }
        if let Some(v) = &s.on_trial {
            rows.push(row("info", format!("{v} is on trial"), "If it crashes before its screen opens, the launcher starts the previous version instead.".into()));
        }
        if let Some(e) = &s.last_error {
            rows.push(row("warn", "The last update check failed".into(), e.clone()));
        }
        let checked = s.last_check.as_deref().map_or("Never checked.".to_string(), |t| format!("Last checked {}.", short_time(t)));
        if let Some(v) = &s.staged {
            rows.push(SetupRow { level: "ok".into(), ..act("upd-unstage", format!("Ferrum {v} is ready"), "Downloaded and checked. It goes into use when LinuxCNC next starts.".into(), "CANCEL", false) });
        } else if let Some(v) = s.newer() {
            rows.push(SetupRow { level: "info".into(), ..act("upd-stage", format!("Ferrum {v} is available"), "Download and check it now; it goes into use when LinuxCNC next starts.".into(), "DOWNLOAD", false) });
        } else if s.last_check.is_none() {
            rows.push(SetupRow { level: "info".into(), ..act("upd-check", "Not checked for updates yet".into(), "Checking asks the release server for the newest version; nothing is downloaded.".into(), "CHECK NOW", false) });
        } else {
            rows.push(SetupRow { level: "ok".into(), ..act("upd-check", "Up to date".into(), checked.clone(), "CHECK NOW", false) });
        }
        rows.push(Self::head("SETTINGS"));
        rows.push(SetupRow {
            kind: "choice".into(),
            key: "upd-channel".into(),
            title: "Updates from".into(),
            detail: "STABLE: tagged releases. BETA: also the builds between releases, every change to main that passed its tests.".into(),
            value: if s.beta() { "BETA" } else { "STABLE" }.into(),
            off: self.updater.busy.is_some(),
            ..Default::default()
        });
        rows.push(SetupRow {
            kind: "bool".into(),
            key: "upd-auto".into(),
            title: "Download updates by themselves".into(),
            detail: if s.beta() {
                "The newest beta build is downloaded and checked in the background, at most once a day, and goes into use only when LinuxCNC next starts."
            } else {
                "A new version is downloaded and checked in the background, and goes into use only when LinuxCNC next starts."
            }
            .into(),
            value: yes(s.auto).into(),
            off: self.updater.busy.is_some(),
            ..Default::default()
        });
        if let Some(p) = &s.previous {
            rows.push(act("upd-rollback", format!("Go back to {p}"), "The version used before this one, still installed. It goes into use when LinuxCNC next starts.".into(), "ROLL BACK", true));
        }
        if s.staged.is_some() || s.newer().is_some() {
            rows.push(act("upd-check", "Check again".into(), checked, "CHECK NOW", false));
        }
        if let Some(v) = &s.skipped {
            rows.push(row("info", format!("{v} is skipped"), "It was rolled back from or crashed, so it is not downloaded by itself. DOWNLOAD still fetches a newer one.".into()));
        }
    }

    fn facts(&self) -> Vec<StatusRow> {
        let (mi, cfg) = (self.mi(), &self.cfg);
        let f = |k: &str, v: String| StatusRow { key: k.into(), value: v.into(), kind: "".into() };
        match self.step() {
            Step::Machine => {
                let mut v = vec![
                    f("UNITS", if mi.units.inch() { "INCH" } else { "MM" }.into()),
                    f("AXES", format!("{}{}", mi.axes.iter().map(|a| a.letter).collect::<String>(), if mi.joints > mi.axes.len() { format!(" · {} JOINTS", mi.joints) } else { String::new() })),
                ];
                for a in mi.axes.iter().filter(|a| !a.rotary).take(3) {
                    let s = format!("AXIS_{}", a.letter);
                    let travel = match (cfg.get_f64(&s, "MIN_LIMIT"), cfg.get_f64(&s, "MAX_LIMIT")) {
                        (Some(lo), Some(hi)) => mi.units.fmt(hi - lo),
                        _ => "NO LIMITS".into(),
                    };
                    v.push(f(&format!("{} TRAVEL", a.letter), travel));
                }
                if let Some(vel) = cfg.get_f64("TRAJ", "MAX_LINEAR_VELOCITY") {
                    v.push(f(&format!("MAX {}/MIN", mi.units.upper()), mi.units.fmt(vel * 60.0).trim_end_matches('0').trim_end_matches('.').to_string()));
                }
                v
            }
            Step::Spindle => vec![
                f("INI TOP SPEED", cfg.get_f64("SPINDLE_0", "MAX_FORWARD_VELOCITY").map_or("NOT SET".into(), |v| format!("{v:.0} RPM"))),
                f("INI REVERSE", cfg.get_f64("SPINDLE_0", "MAX_REVERSE_VELOCITY").map_or("NOT SET".into(), |v| if v == 0.0 { "NONE".into() } else { format!("{v:.0} RPM") })),
            ],
            Step::Software => {
                let ver = env!("CARGO_PKG_VERSION");
                match self.upd.as_ref().and_then(|r| r.as_ref().ok()) {
                    Some(s) => vec![
                        f("IN USE", if s.current.is_empty() { ver.into() } else { s.current.clone() }),
                        f("CHANNEL", s.channel.to_uppercase()),
                        f("INSTALLED", s.installed.len().to_string()),
                        f("LAST CHECK", s.last_check.as_deref().map_or("NEVER".into(), short_time)),
                    ],
                    None => vec![f("IN USE", ver.into()), f("UPDATES", if self.updater.present() { "READING" } else { "NO UPDATER" }.into())],
                }
            }
            _ => Vec::new(),
        }
    }

    /// How many rows need attention: per step, and in all.
    fn counts(&self) -> Vec<i32> {
        STEPS
            .iter()
            .map(|s| {
                let n = self.checks.iter().filter(|c| Step::of(c.group) == *s && c.id != "toolsetter.g30" && matches!(c.level, Level::Attention | Level::Blocks(_))).count();
                let g30 = *s == Step::ToolSetter && self.setter_on() && !g30_set(&self.live.2);
                let upd = *s == Step::Software && self.upd.as_ref().and_then(|r| r.as_ref().ok()).is_some_and(|u| u.fell_back.is_some() || (u.staged.is_none() && u.newer().is_some()));
                (n + g30 as usize + upd as usize) as i32
            })
            .collect()
    }

    /// The saved-changes bar's words.
    fn pending_text(&self) -> String {
        let mut parts = Vec::new();
        let n = self.pending.len();
        if n > 0 {
            parts.push(format!("{n} saved change{}", if n == 1 { "" } else { "s" }));
        }
        let i = self.ini_queue.len();
        if i > 0 {
            parts.push(format!("{i} INI fix{}", if i == 1 { "" } else { "es" }));
        }
        if let Some(v) = self.staged() {
            parts.push(format!("Ferrum {v}"));
        }
        if parts.is_empty() {
            return String::new();
        }
        let list = match parts.len() {
            1 => parts[0].clone(),
            _ => format!("{} and {}", parts[..parts.len() - 1].join(", "), parts[parts.len() - 1]),
        };
        let many = parts.len() > 1 || n > 1 || i > 1;
        format!("{list} {} for LinuxCNC to restart.", if many { "wait" } else { "waits" })
    }

    fn show(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let live = ui.global::<Live>();
        let counts = self.counts();
        let steps: Vec<SetupStep> = STEPS.iter().zip(&counts).map(|(s, n)| SetupStep { label: s.label().into(), count: *n }).collect();
        crate::models::sync(live.get_setup_steps(), steps, |m| live.set_setup_steps(m));
        live.set_setup_step(self.step as i32);
        live.set_setup_title(self.step().label().into());
        live.set_setup_intro(self.step().intro().into());
        crate::models::sync(live.get_setup_facts(), self.facts(), |m| live.set_setup_facts(m));
        crate::models::sync(live.get_setup_rows(), self.rows(), |m| live.set_setup_rows(m));
        live.set_setup_pending(self.pending_text().into());
        live.set_setup_can_restart(self.can_restart);
        // The count on SETUP's nav entry.
        let total: i32 = counts.iter().sum();
        let nav = live.get_nav();
        for i in 0..nav.row_count() {
            if let Some(mut n) = nav.row_data(i) {
                if n.id == "setup" && n.badge != total {
                    n.badge = total;
                    nav.set_row_data(i, n);
                }
            }
        }
    }

    fn report(&self, r: Result<(), String>) {
        if let (Err(e), Some(ui)) = (r, self.ui.upgrade()) {
            let live = ui.global::<Live>();
            live.set_toast_info(false);
            live.set_toast(format!("Not saved: {e}").into());
        }
    }

    /// HAL WIRING's values and the updater's answers, as they arrive.
    fn poll(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let on = ui.global::<Live>().get_screen() == "setup";
        let mut dirty = false;
        let want = on && self.step() == Step::Hal;
        if self.hal.want.swap(want, Ordering::SeqCst) != want && want {
            dirty = true;
        }
        let gen = self.hal.gen.load(Ordering::SeqCst);
        if gen != self.hal_seen {
            self.hal_seen = gen;
            dirty |= want;
        }
        if let Some(r) = self.updater.poll() {
            self.upd_read = Some(Instant::now());
            self.upd_error = r.error.filter(|_| r.op != updater::Op::Status);
            if let Some(status) = r.status {
                self.upd = Some(Ok(status));
            } else if self.upd.is_none() || r.op == updater::Op::Status {
                self.upd = Some(Err(self.upd_error.clone().unwrap_or_else(|| "no answer".into())));
            }
            dirty = true;
        }
        // The staged update shows on the saved-changes bar from any page.
        if self.updater.present() && self.upd.is_none() && self.updater.busy.is_none() {
            self.refresh_updates(true);
        }
        if on && self.step() == Step::Software {
            self.refresh_updates(false);
        }
        if dirty {
            self.show();
        }
    }

    /// Show a page; HAL WIRING and SOFTWARE read what they show.
    fn go(&mut self, i: usize) {
        self.step = i.min(STEPS.len() - 1);
        self.hal.want.store(self.step() == Step::Hal, Ordering::SeqCst);
        if self.step() == Step::Software {
            self.refresh_updates(false);
        }
        self.show();
    }
}

/// The module list as written, else the default, without SETUP and the
/// watchdog (always there) and with old names made current.
fn initial_order(mc: &MachineCfg, cfg: &Config, keep_written: bool) -> Vec<String> {
    let list = if keep_written { mc.module_ids(cfg, crate::modules::DEFAULT_MODULES) } else { crate::machine_cfg::split_words(crate::modules::DEFAULT_MODULES) };
    let mut v: Vec<String> = Vec::new();
    for m in list {
        let m = if m == "jog" { "manual".to_string() } else { m };
        if m == "setup" || m == "watchdog" || v.contains(&m) {
            continue;
        }
        if m == "probe" && mc.probe.enabled == Some(false) {
            continue;
        }
        v.push(m);
    }
    v
}

/// "2026-11-19T14:00:00Z" as "19 Nov 14:00 UTC".
fn short_time(t: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(t) {
        Ok(d) => d.with_timezone(&chrono::Local).format("%-d %b %H:%M").to_string(),
        Err(_) => t.to_string(),
    }
}

/// The settings' keys are static strs; find the one equal to `key`.
fn key_static(key: &str) -> &'static str {
    const KEYS: &[&str] = &[
        "machine.z_top",
        "machine.park",
        "machine.screen_watchdog",
        "spindle.max_rpm",
        "spindle.reverse",
        "buttons.mist",
        "buttons.flood",
        "buttons.drawbar",
        "buttons.home_all",
        "toolchange.style",
        "toolchange.scratch_tools.0",
        "toolchange.scratch_tools.1",
        "probe.enabled",
        "probe.tool",
        "toolsetter.enabled",
        "remote.port",
        "model.show",
        "model.table.0",
        "model.table.1",
        "model.table.2",
        "model.table_center.0",
        "model.table_center.1",
        "model.slots",
        "model.nose_above_table",
        "display.renderer",
        "display.mdi_library",
    ];
    KEYS.iter().copied().find(|k| *k == key).or_else(|| crate::probe_defaults::by_key(key).map(|d| d.key)).unwrap_or("")
}

/// UTIL's G30 text names a position other than machine zero.
fn g30_set(text: &str) -> bool {
    text.split_whitespace().any(|w| w.parse::<f64>().is_ok_and(|v| v != 0.0))
}

fn live_g30(ui: &MainWindow) -> String {
    ui.global::<Live>().get_ut_g30().to_string()
}

fn fmt_num(v: f64) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

struct Keys(Rc<RefCell<Setup>>);

impl crate::manual::KeypadTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        let key = what.strip_prefix("setup-")?;
        let s = self.0.borrow();
        let set = s.setting(key)?;
        let unit = if is_length(key) {
            format!(" ({})", s.mi().units.upper())
        } else if crate::probe_defaults::by_key(key).is_some() {
            format!(" ({})", set.detail.to_uppercase())
        } else {
            String::new()
        };
        Some(format!("{}{unit}", set.title.to_uppercase()))
    }
    fn current(&self, what: &str) -> Option<f64> {
        let key = what.strip_prefix("setup-")?;
        let s = self.0.borrow();
        s.setting(key)?;
        s.value(key).parse().ok()
    }
    fn apply(&self, what: &str, v: f64) -> Option<Result<(), String>> {
        let key = key_static(what.strip_prefix("setup-")?);
        if key.is_empty() {
            return None;
        }
        let mut s = self.0.borrow_mut();
        let r = s.set_number(key, v);
        s.show();
        Some(r)
    }
}

impl crate::textentry::TextTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        let key = what.strip_prefix("setup-")?;
        let s = self.0.borrow();
        if let Some(name) = key.strip_prefix("hal.") {
            return Some(format!("{}: HAL PIN OR SIGNAL", hal_meta(name).0.to_uppercase()));
        }
        Some(s.setting(key).filter(|s| s.kind == Kind::Text)?.title.to_uppercase())
    }
    fn current(&self, what: &str) -> Option<String> {
        let key = what.strip_prefix("setup-")?;
        let s = self.0.borrow();
        let v = s.value(key);
        Some(if v.contains(' ') || v == "NOT CONNECTED" { String::new() } else { v })
    }
    fn apply(&self, what: &str, v: &str) -> Option<Result<(), String>> {
        let key = what.strip_prefix("setup-")?;
        let mut s = self.0.borrow_mut();
        if !key.starts_with("hal.") && s.setting(key).is_none_or(|s| s.kind != Kind::Text) {
            return None;
        }
        let r = s.set_text(key, v);
        s.show();
        Some(r)
    }
}

/// The checker's rows from startup, the modules built and the tool setter's
/// pins. Opens SETUP first when `first` (no ferrum.toml). `can_restart`:
/// ferrum started LinuxCNC (`--boot`, or the simulator), so it can restart it.
/// `hal_live`: read HAL WIRING's values from halcmd. `screenshot`: one frame,
/// read the updater at once.
#[allow(clippy::too_many_arguments)]
pub fn wire(
    ui: &MainWindow,
    cfg: &Config,
    checks: Vec<Check>,
    modules: Vec<String>,
    detected: ToolChangeStyle,
    setter: Option<Rc<SetterHal>>,
    probe: Option<Rc<RefCell<crate::probe::Probe>>>,
    first: bool,
    can_restart: bool,
    hal_live: bool,
    screenshot: bool,
) -> Rc<RefCell<Setup>> {
    let mc = machine_cfg::get();
    let (path, seed) = mc.target(cfg);
    let order = initial_order(mc, cfg, true);
    let hal = Arc::new(HalFeed::default());
    if hal_live {
        let feed = hal.clone();
        std::thread::spawn(move || hal_thread(feed));
    }
    let setup = Rc::new(RefCell::new(Setup {
        ui: ui.as_weak(),
        cfg: cfg.clone(),
        checks,
        settings: settings(cfg),
        modules,
        path,
        seed,
        step: 0,
        pending: BTreeMap::new(),
        picking: None,
        setter,
        detected,
        live: (None, None, live_g30(ui)),
        probe,
        order_running: order.clone(),
        order,
        order_written: mc.is_set("modules", "enabled"),
        hal,
        hal_seen: 0,
        updater: Updater::new(),
        upd: None,
        upd_error: None,
        upd_read: None,
        ini_queue: BTreeSet::new(),
        can_restart,
        timer: None,
    }));
    let live = ui.global::<Live>();
    live.set_setup_first(first);
    if first {
        live.set_screen("setup".into());
    }
    crate::manual::add_keypad_target(Rc::new(Keys(setup.clone())));
    crate::textentry::add_target(Rc::new(Keys(setup.clone())));
    let actions = ui.global::<Actions>();
    let s = setup.clone();
    actions.on_setup_go(move |i| s.borrow_mut().go(i.max(0) as usize));
    let s = setup.clone();
    let weak = ui.as_weak();
    actions.on_setup_tap(move |key| {
        let Some(ui) = weak.upgrade() else { return };
        let key = key.as_str();
        // A key switch ([hal]unlock) locks every change here; the live
        // tests and SOFTWARE's refresh still work.
        if ui.global::<Live>().get_setup_locked() && !matches!(key, "upd-refresh") && !key.starts_with("test") {
            let live = ui.global::<Live>();
            live.set_message("SETUP is locked by the key switch. Turn the key to change settings.".into());
            live.set_message_error(true);
            return;
        }
        match key {
            "set-g30" => return ui.global::<Actions>().invoke_ut_set_g30(),
            "go-g30" => return ui.global::<Actions>().invoke_ut_go_g30(),
            _ => {}
        }
        let mut st = s.borrow_mut();
        let r = if let Some(id) = key.strip_prefix("screen:") {
            st.toggle_module(id)
        } else if let Some(id) = key.strip_prefix("module:") {
            st.toggle_module(id)
        } else if let Some(id) = key.strip_prefix("up:screen:") {
            st.move_screen(id, -1)
        } else if let Some(id) = key.strip_prefix("down:screen:") {
            st.move_screen(id, 1)
        } else if key == "screens-default" {
            st.reset_order()
        } else if key == "convert-postgui" {
            st.convert_postgui()
        } else if let Some(id) = key.strip_prefix("ini:") {
            if let Some(c) = st.checks.iter().find(|c| c.id == id).map(|c| c.id) {
                if !st.ini_queue.remove(c) {
                    st.ini_queue.insert(c);
                }
            }
            Ok(())
        } else if let Some(name) = key.strip_prefix("hal.") {
            let name = name.to_string();
            st.open_hal_picker(&ui, &name);
            Ok(())
        } else if key == "upd-channel" {
            let current = st.upd.as_ref().and_then(|r| r.as_ref().ok()).map(|u| u.beta() as usize);
            st.picking = Some(Pick::Channel);
            drop(st);
            crate::picker::open(&ui, "setup", "UPDATES FROM", vec!["STABLE".into(), "BETA".into()], current);
            return;
        } else if key == "upd-refresh" {
            st.refresh_updates(true);
            Ok(())
        } else if key.starts_with("upd-") {
            st.update_action(key);
            Ok(())
        } else {
            let key = key_static(key);
            let Some(set) = st.setting(key).cloned() else { return };
            match set.kind {
                Kind::Bool => st.toggle(key),
                Kind::Choice(items) => {
                    let current = items.iter().position(|(_, l)| st.value(key).ends_with(l));
                    st.picking = Some(Pick::Setting(key));
                    drop(st);
                    crate::picker::open(&ui, "setup", &set.title.to_uppercase(), items.iter().map(|(_, l)| l.to_string()).collect(), current);
                    return;
                }
                Kind::Text => {
                    drop(st);
                    ui.global::<Actions>().invoke_text_edit(format!("setup-{key}").into());
                    return;
                }
                // The row opens the keypad itself.
                Kind::Number { .. } => Ok(()),
            }
        };
        st.report(r);
        st.show();
    });
    let s = setup.clone();
    let weak = ui.as_weak();
    crate::picker::on_pick("setup", move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = s.borrow_mut();
        let r = match st.picking.take() {
            Some(Pick::Setting(key)) => st.choose(key, i),
            Some(Pick::Hal(name, targets)) => match i {
                0 => st.set_hal(&name, None),
                1 => {
                    drop(st);
                    ui.global::<Actions>().invoke_text_edit(format!("setup-hal.{name}").into());
                    return;
                }
                _ => match targets.get(i - 2) {
                    Some(t) => {
                        let t = t.clone();
                        st.set_hal(&name, Some(&t))
                    }
                    None => Ok(()),
                },
            },
            Some(Pick::Channel) => {
                let beta = st.upd.as_ref().and_then(|r| r.as_ref().ok()).is_some_and(|u| u.beta());
                if (i == 1) != beta {
                    st.update_action("upd-channel");
                }
                Ok(())
            }
            None => Ok(()),
        };
        st.report(r);
        st.show();
    });
    let s = setup.clone();
    let weak = ui.as_weak();
    actions.on_setup_finish(move || {
        let Some(ui) = weak.upgrade() else { return };
        let r = s.borrow_mut().finish();
        let ok = r.is_ok();
        s.borrow().report(r);
        if ok {
            let live = ui.global::<Live>();
            live.set_setup_first(false);
            let first = live.get_nav().row_data(0).map(|n| n.id).unwrap_or_else(|| "run".into());
            ui.global::<Actions>().invoke_navigate(first);
        }
    });
    if !screenshot {
        let t = slint::Timer::default();
        let s = Rc::downgrade(&setup);
        t.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
            if let Some(s) = s.upgrade() {
                if let Ok(mut st) = s.try_borrow_mut() {
                    st.poll();
                }
            }
        });
        setup.borrow_mut().timer = Some(t);
    }
    {
        let mut st = setup.borrow_mut();
        // Screenshots draw at once: read the updater now.
        if screenshot && st.updater.present() {
            st.upd = st.updater.status_now();
            st.upd_read = Some(Instant::now());
        }
        st.show();
    }
    setup
}

/// Read HAL every second while HAL WIRING wants it.
fn hal_thread(feed: Arc<HalFeed>) {
    let mut last: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if !feed.want.load(Ordering::SeqCst) || last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            continue;
        }
        last = Some(Instant::now());
        let items = crate::halmap::snapshot();
        if let Ok(mut g) = feed.items.lock() {
            if *g != items {
                *g = items;
                feed.gen.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

/// The live tests follow the probe and setter inputs and the G30 position;
/// HAL WIRING and SOFTWARE pick up what their threads read.
pub fn apply_status(ui: &MainWindow, setup: &Rc<RefCell<Setup>>, s: &Status) {
    let mut st = setup.borrow_mut();
    let live = ui.global::<Live>();
    // The inputs only matter on SETUP; the G30 position also sets the count.
    let on = live.get_screen() == "setup";
    let setter = st.setter.as_ref().filter(|h| h.trip_wired.get()).map(|h| h.triggered.get());
    let now = (on.then_some(s.probe_val), setter.filter(|_| on), live.get_ut_g30().to_string());
    if now != st.live {
        st.live = now;
        st.show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_comments_and_refuse_bad_values() {
        let text = "# my mill\n[spindle]\nmax_rpm = 3000 # belt\n\n[model]\nshow = true\n";
        let out = edited(text, "spindle.max_rpm", Some(toml_edit::value(4500.0))).unwrap();
        assert!(out.contains("# my mill") && out.contains("max_rpm = 4500") && out.contains("[model]"), "{out}");
        let out = edited(&out, "probe.tool", Some(toml_edit::value(12i64))).unwrap();
        assert!(out.contains("[probe]\ntool = 12"), "{out}");
        let out = edited(&out, "spindle.max_rpm", None).unwrap();
        assert!(!out.contains("max_rpm") && out.contains("[spindle]"), "{out}");
        assert!(edited(&out, "machine.park", Some(toml_edit::value("nowhere"))).is_err());
        let (c, p) = MachineCfg::parse(&out);
        assert!(p.is_empty(), "{p:?}");
        assert_eq!(c.probe.tool, Some(12));
    }

    #[test]
    fn steps_cover_every_group() {
        for g in [Group::Config, Group::Machine, Group::Files, Group::Spindle, Group::ToolChange, Group::Probe, Group::ToolSetter, Group::Hal, Group::Job, Group::Display] {
            assert!(STEPS.contains(&Step::of(g)));
        }
    }

    #[test]
    fn every_setting_key_is_known() {
        let cfg = Config::default();
        for s in settings(&cfg) {
            assert_eq!(key_static(s.key), s.key);
            let key = indexed(s.key).map_or(s.key, |(b, _)| b);
            // Each key is one ferrum.toml reads.
            let v = match s.kind {
                Kind::Bool => toml_edit::value(true),
                Kind::Number { whole: true, .. } => toml_edit::value(1i64),
                Kind::Number { .. } => toml_edit::value(1.0),
                Kind::Choice(c) => toml_edit::value(c.iter().find(|(v, _)| !v.is_empty()).unwrap().0),
                Kind::Text => toml_edit::value("x"),
            };
            let v = if indexed(s.key).is_some() {
                let mut a = toml_edit::Array::new();
                let n = if key == "model.table" { 3 } else { 2 };
                for _ in 0..n {
                    if matches!(s.kind, Kind::Number { whole: true, .. }) {
                        a.push(1i64);
                    } else {
                        a.push(1.0);
                    }
                }
                toml_edit::value(a)
            } else if s.key == "machine.z_top" || s.key == "spindle.max_rpm" {
                toml_edit::value(1.0)
            } else {
                v
            };
            edited("", key, Some(v)).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
    }

    #[test]
    fn every_hal_name_has_a_title() {
        for h in all_hal_names() {
            assert!(!hal_meta(h.name).0.is_empty(), "{}", h.name);
        }
    }

    #[test]
    fn indexed_keys() {
        assert_eq!(indexed("model.table.2"), Some(("model.table", 2)));
        assert_eq!(indexed("model.show"), None);
    }
}
