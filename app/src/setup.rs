//! SETUP: the configuration checker's rows in six guided steps, the
//! millgui.toml settings each step asks for, and live tests of the probe
//! and tool setter inputs.
//!
//! A setting is written to millgui.toml at once (comments kept). millgui
//! reads its settings when it starts, as LinuxCNC reads the INI, so a
//! saved change shows as waiting until LinuxCNC (and with it millgui) next
//! starts. With no millgui.toml, SETUP opens first; FINISH writes the file.

use crate::checks::{Check, Fix, Group, Level};
use crate::config::Config;
use crate::machine_cfg::{self, MachineCfg, Park, ToolChangeStyle};
use crate::machine_info::{self, MachineInfo};
use crate::modules::SetterHal;
use crate::{Actions, Live, MainWindow, SetupRow, SetupStep, StatusRow};
use machine::Status;
use slint::{ComponentHandle, Model, SharedString};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Machine,
    Spindle,
    ToolChange,
    Probe,
    ToolSetter,
    Extras,
}

pub const STEPS: [Step; 6] = [Step::Machine, Step::Spindle, Step::ToolChange, Step::Probe, Step::ToolSetter, Step::Extras];

impl Step {
    fn label(self) -> &'static str {
        match self {
            Step::Machine => "MACHINE",
            Step::Spindle => "SPINDLE",
            Step::ToolChange => "TOOL CHANGE",
            Step::Probe => "PROBE",
            Step::ToolSetter => "TOOL SETTER",
            Step::Extras => "EXTRAS",
        }
    }

    fn intro(self) -> &'static str {
        match self {
            Step::Machine => "What millgui read from the INI. Check the units and travel, and say where retracts and PARK go.",
            Step::Spindle => "Every speed millgui sends is checked against the top speed. CCW shows only when the spindle reverses.",
            Step::ToolChange => "Who answers M6. millgui works it out from HAL; set it here only when that is wrong.",
            Step::Probe => "PROBE is on only with a probe. Touch the probe to test its input before the first probe move.",
            Step::ToolSetter => "Touch-off goes to the G30 position and probes down. Jog over the setter on MANUAL, then set the position here.",
            Step::Extras => "HAL wiring, jobs and conversational, and the browser screen.",
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
            Group::Hal | Group::Job | Group::Display => Step::Extras,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Number { min: f64, max: f64, whole: bool },
    Bool,
    /// (value written, label shown); the value "" removes the key.
    Choice(&'static [(&'static str, &'static str)]),
}

#[derive(Clone, Debug)]
struct Setting {
    key: &'static str,
    step: Step,
    title: &'static str,
    detail: &'static str,
    kind: Kind,
}

const PARK: &[(&str, &str)] = &[("machine-zero", "MACHINE X0 Y0"), ("g28", "G28 POSITION")];
const STYLE: &[(&str, &str)] =
    &[("", "DETECT FROM HAL"), ("external", "THE MACHINE (HAL)"), ("dialog", "MILLGUI'S DIALOG"), ("drawbar", "DIALOG + DRAWBAR")];

/// The settings SETUP asks for, with this machine's ranges.
fn settings(cfg: &Config) -> Vec<Setting> {
    let zmin = cfg.get_f64("AXIS_Z", "MIN_LIMIT").unwrap_or(f64::MIN);
    let zmax = cfg.get_f64("AXIS_Z", "MAX_LIMIT").unwrap_or(f64::MAX);
    vec![
        Setting { key: "machine.z_top", step: Step::Machine, title: "Retract height (machine Z)", detail: "Every retract and PARK goes up to it first.", kind: Kind::Number { min: zmin, max: zmax, whole: false } },
        Setting { key: "machine.park", step: Step::Machine, title: "PARK goes to", detail: "After Z is up.", kind: Kind::Choice(PARK) },
        Setting { key: "spindle.max_rpm", step: Step::Spindle, title: "Top speed (rpm)", detail: "Faster speeds are refused.", kind: Kind::Number { min: 1.0, max: 100_000.0, whole: true } },
        Setting { key: "spindle.reverse", step: Step::Spindle, title: "The spindle runs in reverse (M4)", detail: "Shows CCW on MANUAL.", kind: Kind::Bool },
        Setting { key: "buttons.mist", step: Step::Spindle, title: "MIST button", detail: "Under the overrides.", kind: Kind::Bool },
        Setting { key: "buttons.flood", step: Step::Spindle, title: "FLOOD button", detail: "Under the overrides.", kind: Kind::Bool },
        Setting { key: "toolchange.style", step: Step::ToolChange, title: "M6 is answered by", detail: "", kind: Kind::Choice(STYLE) },
        Setting { key: "probe.enabled", step: Step::Probe, title: "This machine has a probe", detail: "Without one, PROBE leaves the nav bar.", kind: Kind::Bool },
        Setting { key: "probe.tool", step: Step::Probe, title: "Probe tool number", detail: "The spindle cannot start with it loaded.", kind: Kind::Number { min: 1.0, max: 99_999.0, whole: true } },
        Setting { key: "toolsetter.enabled", step: Step::ToolSetter, title: "This machine has a tool setter", detail: "", kind: Kind::Bool },
        Setting { key: "remote.port", step: Step::Extras, title: "Browser screen port", detail: "0 turns it off. Anyone who can reach the port can run the machine.", kind: Kind::Number { min: 0.0, max: 65_535.0, whole: true } },
    ]
}

fn yes(b: bool) -> String {
    if b { "YES" } else { "NO" }.into()
}

fn style_label(s: ToolChangeStyle) -> &'static str {
    match s {
        ToolChangeStyle::External => "THE MACHINE (HAL)",
        ToolChangeStyle::Dialog => "MILLGUI'S DIALOG",
        ToolChangeStyle::Drawbar => "DIALOG + DRAWBAR",
    }
}

/// `text` with `[table]key` set to `value`, or removed for None. Comments
/// and layout are kept. Refused when the result would not read cleanly.
pub fn edited(text: &str, key: &str, value: Option<toml_edit::Item>) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| format!("millgui.toml does not read: {}", e.message()))?;
    let (table, k) = key.split_once('.').ok_or("bad key")?;
    match value {
        Some(v) => {
            let t = doc.as_table_mut().entry(table).or_insert(toml_edit::table());
            let t = t.as_table_like_mut().ok_or_else(|| format!("[{table}] in millgui.toml is not a table"))?;
            t.insert(k, v);
        }
        None => {
            if let Some(t) = doc.get_mut(table).and_then(|t| t.as_table_like_mut()) {
                t.remove(k);
            }
        }
    }
    let out = doc.to_string();
    let (_, problems) = MachineCfg::parse(&out);
    if let Some(p) = problems.iter().find(|p| p.starts_with(&format!("[{table}]"))) {
        return Err(p.clone());
    }
    Ok(out)
}

const HEADER: &str = "# millgui's settings for this machine (millgui/docs/millgui-toml.md).\n# SETUP writes what it changes here; every key is optional.\n";

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
    pending: BTreeMap<&'static str, String>,
    /// The setting a picker is open for.
    picking: Option<&'static str>,
    setter: Option<Rc<SetterHal>>,
    /// Who answers M6 as HAL has it, whatever [toolchange]style says.
    detected: ToolChangeStyle,
    /// Live test inputs last shown: probe, setter, G30 text.
    live: (Option<bool>, Option<bool>, String),
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

    /// The setting's value as millgui runs now.
    fn running(&self, key: &str) -> String {
        let (mc, mi, cfg) = (self.mc(), self.mi(), &self.cfg);
        let ui = self.ui.upgrade();
        let live = ui.as_ref().map(|ui| ui.global::<Live>());
        match key {
            "machine.z_top" => mi.units.fmt(mi.z_top),
            "machine.park" => match mc.machine.park {
                Park::MachineZero => PARK[0].1.into(),
                Park::G28 => PARK[1].1.into(),
                Park::Xy(x, y) => format!("X{} Y{}", mi.units.fmt(x), mi.units.fmt(y)),
            },
            "spindle.max_rpm" => mi.max_rpm.map_or("NOT SET".into(), |v| format!("{v:.0}")),
            "spindle.reverse" => yes(mi.reverse),
            "buttons.mist" => yes(live.as_ref().map_or(mc.buttons.mist, |l| l.get_quick_mist())),
            "buttons.flood" => yes(live.as_ref().map_or(mc.buttons.flood, |l| l.get_quick_flood())),
            "toolchange.style" => match mc.toolchange.style {
                Some(s) => style_label(s).into(),
                None => format!("DETECTED: {}", style_label(self.detected)),
            },
            "probe.enabled" => yes(self.has("probe")),
            "probe.tool" => mc.probe_tool(cfg).to_string(),
            "toolsetter.enabled" => yes(self.setter_on()),
            "remote.port" => match mc.remote_port(cfg) {
                Some(Ok(p)) => p.to_string(),
                _ => "0".into(),
            },
            _ => String::new(),
        }
    }

    fn value(&self, key: &str) -> String {
        self.pending.get(key).cloned().unwrap_or_else(|| self.running(key))
    }

    fn setting(&self, key: &str) -> Option<&Setting> {
        self.settings.iter().find(|s| s.key == key)
    }

    /// Write one key; the shown value waits for the next start.
    fn save(&mut self, key: &'static str, value: Option<toml_edit::Item>, shown: String) -> Result<(), String> {
        // A new file: the older machine.toml's settings, under a header.
        let (text, new) = match std::fs::read_to_string(&self.path) {
            Ok(t) => (t, false),
            Err(_) => (self.seed.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default(), true),
        };
        let out = edited(&text, key, value)?;
        let out = if new { format!("{HEADER}\n{out}") } else { out };
        write_file(&self.path, &out)?;
        if shown == self.running(key) {
            self.pending.remove(key);
        } else {
            self.pending.insert(key, shown);
        }
        Ok(())
    }

    /// A new millgui.toml: the older machine.toml's settings, under a header.
    fn new_file_text(&self) -> String {
        let old = self.seed.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        if old.is_empty() { HEADER.to_string() } else { format!("{HEADER}\n{old}") }
    }

    fn set_number(&mut self, key: &'static str, v: f64) -> Result<(), String> {
        let Some(s) = self.setting(key) else { return Err("not a setting".into()) };
        let Kind::Number { min, max, whole } = s.kind else { return Err("not a number".into()) };
        if !(min..=max).contains(&v) || (whole && v.fract() != 0.0) {
            let unit = if key == "machine.z_top" { format!(" {}", self.mi().units.name()) } else { String::new() };
            let range = if min > f64::MIN && max < f64::MAX { format!(": {} to {}{unit}", fmt_num(min), fmt_num(max)) } else { String::new() };
            return Err(format!("Out of range{range}{}", if whole { ", whole numbers" } else { "" }));
        }
        let (item, shown) = match key {
            "machine.z_top" => (toml_edit::value(v), self.mi().units.fmt(v)),
            _ => (toml_edit::value(v as i64), format!("{}", v as i64)),
        };
        self.save(key, Some(item), shown)
    }

    fn toggle(&mut self, key: &'static str) -> Result<(), String> {
        let now = self.value(key) == "YES";
        self.save(key, Some(toml_edit::value(!now)), yes(!now))
    }

    fn choose(&mut self, key: &'static str, i: usize) -> Result<(), String> {
        let Some(Kind::Choice(items)) = self.setting(key).map(|s| s.kind) else { return Err("not a choice".into()) };
        let (v, label) = items.get(i).ok_or("no such choice")?;
        let shown = if v.is_empty() { format!("DETECTED: {}", style_label(self.detected)) } else { label.to_string() };
        self.save(key, if v.is_empty() { None } else { Some(toml_edit::value(*v)) }, shown)
    }

    /// FINISH: make sure millgui.toml exists, so SETUP stops opening first.
    fn finish(&mut self) -> Result<(), String> {
        if !self.path.exists() {
            write_file(&self.path, &self.new_file_text())?;
        }
        Ok(())
    }

    fn step(&self) -> Step {
        STEPS[self.step]
    }

    /// The rows of the current step: its settings (titled by the check
    /// about them, if any), its live tests, then its checks, worst first.
    fn rows(&self) -> Vec<SetupRow> {
        let step = self.step();
        let mut rows = Vec::new();
        let level = |l: &Level| match l {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Attention => "warn",
            Level::Blocks(_) => "bad",
        };
        let about = |key: &str| self.checks.iter().find(|c| c.fix == Fix::Setting(key_static(key)));
        for s in self.settings.iter().filter(|s| s.step == step) {
            // The probe tool only matters with a probe.
            if s.key == "probe.tool" && !self.has("probe") {
                continue;
            }
            let check = about(s.key);
            let kind = match s.kind {
                Kind::Number { .. } => "number",
                Kind::Bool => "bool",
                Kind::Choice(_) => "choice",
            };
            // A setting that needs attention says why; then what was found.
            let mut detail = match check {
                Some(c) if matches!(c.level, Level::Attention | Level::Blocks(_)) => format!("{}.", c.title),
                _ => s.detail.to_string(),
            };
            if let Some(c) = check.filter(|c| !c.detail.is_empty()) {
                detail = format!("{detail} {}", c.detail).trim().to_string();
            }
            let pending = self.pending.contains_key(s.key);
            rows.push(SetupRow {
                kind: kind.into(),
                key: s.key.into(),
                level: if pending { "warn" } else { check.map_or("", |c| level(&c.level)) }.into(),
                title: s.title.into(),
                detail: detail.into(),
                value: self.value(s.key).into(),
                code: "".into(),
                note: if pending { "Saved. Applies when LinuxCNC next starts." } else { "" }.into(),
            });
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
                code: "".into(),
                note: "".into(),
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
                        "millgui does not see the setter's input. Name it in [hal]toolsetter_tripped (a HAL pin or signal) to see it here and on UTIL."
                    }
                    .into(),
                    value: match (wired, setter) {
                        (false, _) => "NOT WIRED",
                        (_, Some(true)) => "TRIPPED",
                        _ => "OPEN",
                    }
                    .into(),
                    code: if wired { "" } else { "[hal]\ntoolsetter_tripped = \"<your setter's input pin>\"" }.into(),
                    note: "".into(),
                });
                rows.push(SetupRow {
                    kind: "action".into(),
                    key: "set-g30".into(),
                    level: if g30_set(g30) { "ok" } else { "warn" }.into(),
                    title: format!("Setter position: {g30}").into(),
                    detail: "Jog over the setter on MANUAL with the spindle clear, come back and hold SET POSITION HERE (G30.1).".into(),
                    value: "SET POSITION HERE".into(),
                    code: "".into(),
                    note: "".into(),
                });
                rows.push(SetupRow {
                    kind: "action".into(),
                    key: "go-g30".into(),
                    level: "".into(),
                    title: "Check the position".into(),
                    detail: "Retracts Z, then moves over the setter.".into(),
                    value: "GO TO G30".into(),
                    code: "".into(),
                    note: "".into(),
                });
            }
            _ => {}
        }
        let mut checks: Vec<&Check> = self
            .checks
            .iter()
            .filter(|c| Step::of(c.group) == step)
            .filter(|c| !matches!(&c.fix, Fix::Setting(k) if self.setting(k).is_some()))
            // The setter position row above follows SET POSITION HERE.
            .filter(|c| c.id != "toolsetter.g30")
            // The banner says it on a first run.
            .filter(|c| !(c.id == "config.file" && c.level == Level::Info))
            .collect();
        let rank = |c: &Check| match c.level {
            Level::Blocks(_) => 0,
            Level::Attention => 1,
            Level::Info => 2,
            Level::Ok => 3,
        };
        checks.sort_by_key(|c| rank(c));
        for c in checks {
            let (code, how) = match &c.fix {
                Fix::IniLines { section, lines } => (format!("[{section}]\n{}", lines.join("\n")), "Add to the INI, then restart LinuxCNC."),
                Fix::Hal(line) => (line.clone(), "Add to the machine's HAL file."),
                _ => (String::new(), ""),
            };
            let detail = [c.detail.as_str(), how].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
            rows.push(SetupRow {
                kind: "check".into(),
                key: c.id.into(),
                level: level(&c.level).into(),
                title: c.title.clone().into(),
                detail: detail.into(),
                value: "".into(),
                code: code.into(),
                note: "".into(),
            });
        }
        rows
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
                (n + g30 as usize) as i32
            })
            .collect()
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
        live.set_setup_pending(match self.pending.len() {
            0 => SharedString::new(),
            n => format!("{n} saved change{} in {}. {} when LinuxCNC next starts.", if n == 1 { "" } else { "s" }, self.path.display(), if n == 1 { "It applies" } else { "They apply" }).into(),
        });
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
}

/// The settings' keys are static strs; find the one equal to `key`.
fn key_static(key: &str) -> &'static str {
    const KEYS: &[&str] = &[
        "machine.z_top",
        "machine.park",
        "spindle.max_rpm",
        "spindle.reverse",
        "buttons.mist",
        "buttons.flood",
        "toolchange.style",
        "probe.enabled",
        "probe.tool",
        "toolsetter.enabled",
        "remote.port",
    ];
    KEYS.iter().copied().find(|k| *k == key).unwrap_or("")
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

/// Write through a temporary file, so a crash never leaves half a file.
fn write_file(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

struct Keys(Rc<RefCell<Setup>>);

impl crate::manual::KeypadTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        let key = what.strip_prefix("setup-")?;
        let s = self.0.borrow();
        let set = s.setting(key)?;
        let unit = if key == "machine.z_top" { format!(" ({})", s.mi().units.upper()) } else { String::new() };
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

/// The checker's rows from startup, the modules built and the tool setter's
/// pins. Opens SETUP first when `first` (no millgui.toml).
pub fn wire(ui: &MainWindow, cfg: &Config, checks: Vec<Check>, modules: Vec<String>, detected: ToolChangeStyle, setter: Option<Rc<SetterHal>>, first: bool) -> Rc<RefCell<Setup>> {
    let mc = machine_cfg::get();
    let (path, seed) = mc.target(cfg);
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
    }));
    let live = ui.global::<Live>();
    live.set_setup_first(first);
    if first {
        live.set_screen("setup".into());
    }
    crate::manual::add_keypad_target(Rc::new(Keys(setup.clone())));
    let actions = ui.global::<Actions>();
    let s = setup.clone();
    actions.on_setup_go(move |i| {
        let mut s = s.borrow_mut();
        s.step = (i.max(0) as usize).min(STEPS.len() - 1);
        s.show();
    });
    let s = setup.clone();
    let weak = ui.as_weak();
    actions.on_setup_tap(move |key| {
        let Some(ui) = weak.upgrade() else { return };
        let key = key.as_str();
        match key {
            "set-g30" => return ui.global::<Actions>().invoke_ut_set_g30(),
            "go-g30" => return ui.global::<Actions>().invoke_ut_go_g30(),
            _ => {}
        }
        let key = key_static(key);
        let mut st = s.borrow_mut();
        let Some(set) = st.setting(key).cloned() else { return };
        match set.kind {
            Kind::Bool => {
                let r = st.toggle(key);
                st.report(r);
                st.show();
            }
            Kind::Choice(items) => {
                let current = items.iter().position(|(_, l)| st.value(key).ends_with(l));
                st.picking = Some(key);
                drop(st);
                crate::picker::open(&ui, "setup", &set.title.to_uppercase(), items.iter().map(|(_, l)| l.to_string()).collect(), current);
            }
            // The row opens the keypad itself.
            Kind::Number { .. } => {}
        }
    });
    let s = setup.clone();
    crate::picker::on_pick("setup", move |i| {
        let mut st = s.borrow_mut();
        if let Some(key) = st.picking.take() {
            let r = st.choose(key, i);
            st.report(r);
            st.show();
        }
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
    setup.borrow().show();
    setup
}

/// The live tests follow the probe and setter inputs and the G30 position.
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
            let (table, key) = s.key.split_once('.').unwrap();
            // Each key is one millgui.toml reads.
            let v = match s.kind {
                Kind::Bool => toml_edit::value(true),
                Kind::Number { .. } => toml_edit::value(1i64),
                Kind::Choice(c) => toml_edit::value(c.iter().find(|(v, _)| !v.is_empty()).unwrap().0),
            };
            let v = if s.key == "machine.z_top" || s.key == "spindle.max_rpm" { toml_edit::value(1.0) } else { v };
            edited("", s.key, Some(v)).unwrap_or_else(|e| panic!("{table}.{key}: {e}"));
        }
    }
}
