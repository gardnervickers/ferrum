//! The configuration checker: what ferrum read, what it could not find and
//! what that switches off. One list of rows, used three ways: `ferrum
//! --check <ini>` prints it, startup uses it to switch features off with a
//! reason, and (phase 3) the SETUP screen shows it with fixes.

use crate::config::Config;
use crate::machine_cfg::{MachineCfg, ToolChangeStyle};
use crate::machine_info::MachineInfo;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Config,
    Machine,
    Files,
    Spindle,
    ToolChange,
    Probe,
    ToolSetter,
    Hal,
    Job,
    Display,
}

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Group::Config => "ferrum.toml",
            Group::Machine => "Machine",
            Group::Files => "Files",
            Group::Spindle => "Spindle",
            Group::ToolChange => "Tool change",
            Group::Probe => "Probe",
            Group::ToolSetter => "Tool setter",
            Group::Hal => "HAL",
            Group::Job => "Job and conversational",
            Group::Display => "Display",
        }
    }
}

/// A feature a row can switch off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// Everything: ferrum cannot run the machine.
    Core,
    Probe,
    Job,
    Conv,
    Preview,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Level {
    Ok,
    Info,
    Attention,
    Blocks(Vec<Feature>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    None,
    /// A ferrum.toml key.
    Setting(&'static str),
    /// Lines for an INI section; LinuxCNC must be restarted.
    IniLines { section: &'static str, lines: Vec<String> },
    /// What to add to the machine's HAL.
    Hal(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub id: &'static str,
    pub group: Group,
    pub level: Level,
    pub title: String,
    pub detail: String,
    pub fix: Fix,
}

impl Check {
    fn new(id: &'static str, group: Group, level: Level, title: impl Into<String>) -> Self {
        Check { id, group, level, title: title.into(), detail: String::new(), fix: Fix::None }
    }
    fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = d.into();
        self
    }
    fn fix(mut self, f: Fix) -> Self {
        self.fix = f;
        self
    }
    pub fn blocks(&self, f: Feature) -> bool {
        matches!(&self.level, Level::Blocks(v) if v.contains(&f))
    }
}

/// What HAL says about a pin or signal. Implemented with halcmd, and by a
/// table in tests.
pub trait Hal {
    /// None: HAL is not reachable (LinuxCNC not running). Some(None): the
    /// pin does not exist. Some(Some(sig)): its signal ("" when unconnected).
    fn pin_signal(&self, pin: &str) -> Option<Option<String>>;
    /// The pin writing a signal, if any (None also when HAL is unreachable).
    fn signal_writer(&self, sig: &str) -> Option<String>;
}

/// `halcmd -s show pin|sig`, as the user running LinuxCNC.
pub struct Halcmd {
    up: bool,
}

impl Halcmd {
    pub fn new() -> Self {
        // `show comp` lists motmod while LinuxCNC runs.
        let up = run_halcmd(&["-s", "show", "comp"]).is_some_and(|o| o.split_whitespace().any(|w| w == "motmod"));
        Halcmd { up }
    }
}

impl Halcmd {
    /// LinuxCNC is running (its HAL is up).
    pub fn running(&self) -> bool {
        self.up
    }
}

fn run_halcmd(args: &[&str]) -> Option<String> {
    let out = Command::new("halcmd").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `halcmd -s show pin NAME`: "owner type dir value name [<==|==>|<=> sig]".
pub fn parse_pin(out: &str, pin: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        let at = w.iter().position(|x| *x == pin)?;
        Some(w.get(at + 2).filter(|_| w.get(at + 1).is_some_and(|a| a.contains("=="))).map_or(String::new(), |s| s.to_string()))
    })
}

/// `halcmd -s show sig NAME`: "type value name ==> pin <== pin ...".
pub fn parse_writer(out: &str, sig: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        w.get(2).filter(|n| **n == sig)?;
        w.windows(2).find(|p| p[0] == "<==" || p[0] == "<=>").map(|p| p[1].to_string())
    })
}

impl Hal for Halcmd {
    fn pin_signal(&self, pin: &str) -> Option<Option<String>> {
        if !self.up {
            return None;
        }
        Some(run_halcmd(&["-s", "show", "pin", pin]).and_then(|o| parse_pin(&o, pin)))
    }
    fn signal_writer(&self, sig: &str) -> Option<String> {
        if !self.up || sig.is_empty() {
            return None;
        }
        run_halcmd(&["-s", "show", "sig", sig]).and_then(|o| parse_writer(&o, sig))
    }
}

/// No HAL: the simulator, tests.
pub struct NoHal;

impl Hal for NoHal {
    fn pin_signal(&self, _: &str) -> Option<Option<String>> {
        None
    }
    fn signal_writer(&self, _: &str) -> Option<String> {
        None
    }
}

/// Who answers M6, and why.
pub fn toolchange_style(mc: &MachineCfg, modules: &[String], hal: &dyn Hal) -> (ToolChangeStyle, String) {
    if let Some(s) = mc.toolchange.style {
        return (s, "Set in [toolchange]style.".into());
    }
    if modules.iter().any(|m| m == "drawbar") {
        return (ToolChangeStyle::Drawbar, "The drawbar module is on.".into());
    }
    match hal.pin_signal("iocontrol.0.tool-changed") {
        None => (ToolChangeStyle::Dialog, "LinuxCNC is not running, so ferrum assumes nothing else answers M6.".into()),
        Some(None) => (ToolChangeStyle::Dialog, "iocontrol.0.tool-changed was not found.".into()),
        Some(Some(sig)) => match hal.signal_writer(&sig) {
            Some(w) if !w.starts_with("ferrum.") => (ToolChangeStyle::External, format!("{w} answers M6 (signal {sig}).")),
            _ => (ToolChangeStyle::Dialog, "Nothing in the machine's HAL answers iocontrol.0.tool-changed.".into()),
        },
    }
}

/// Everything the checker looks at.
pub struct Inputs<'a> {
    pub cfg: &'a Config,
    pub mc: &'a MachineCfg,
    pub mi: &'a MachineInfo,
    /// Problems found reading ferrum.toml (and the INI).
    pub notes: &'a [String],
    pub modules: &'a [String],
    pub hal: &'a dyn Hal,
}

fn resolve(cfg: &Config, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() { p.to_path_buf() } else { cfg.dir().join(p) }
}

/// The folders LinuxCNC looks in for `o<name>` subroutines.
pub fn subroutine_dirs(cfg: &Config) -> Vec<PathBuf> {
    cfg.get("RS274NGC", "SUBROUTINE_PATH")
        .map(|v| v.split(':').map(str::trim).filter(|d| !d.is_empty()).map(|d| resolve(cfg, d)).collect())
        .unwrap_or_default()
}

/// The subroutines of `names` that no folder on SUBROUTINE_PATH has.
pub fn missing_subs<'n>(cfg: &Config, names: &[&'n str]) -> Vec<&'n str> {
    let dirs = subroutine_dirs(cfg);
    names.iter().copied().filter(|n| !dirs.iter().any(|d| d.join(format!("{n}.ngc")).is_file())).collect()
}

fn on(v: Option<&str>) -> bool {
    v.is_some_and(|v| matches!(v.trim(), "1" | "yes" | "YES" | "true" | "TRUE"))
}

pub fn run(i: &Inputs) -> Vec<Check> {
    let (cfg, mc, mi) = (i.cfg, i.mc, i.mi);
    let mut v = Vec::new();
    use Group as G;
    use Level as L;

    // ferrum.toml
    match &mc.path {
        Some(p) => v.push(Check::new("config.file", G::Config, L::Ok, format!("Settings read from {}", p.display()))),
        None => v.push(
            Check::new("config.file", G::Config, L::Info, "No ferrum.toml: using what ferrum detects")
                .detail(format!("Create {} to change a setting.", cfg.dir().join(crate::machine_cfg::FILE).display())),
        ),
    }
    for n in i.notes {
        v.push(Check::new("config.problem", G::Config, L::Attention, n.clone()));
    }

    // Machine
    if cfg.path.is_none() {
        v.push(Check::new("machine.ini", G::Machine, L::Blocks(vec![Feature::Core]), "No INI file").detail("Start ferrum from LinuxCNC, or pass -ini <file>."));
        return v;
    }
    match &mi.units_unknown {
        Some(u) => v.push(
            Check::new("machine.units", G::Machine, L::Attention, format!("[TRAJ]LINEAR_UNITS = {u} is not a unit ferrum knows"))
                .detail("ferrum shows millimetres. Use mm or inch.")
                .fix(Fix::IniLines { section: "TRAJ", lines: vec!["LINEAR_UNITS = mm".into()] }),
        ),
        None => v.push(Check::new(
            "machine.units",
            G::Machine,
            L::Ok,
            format!("{} machine, axes {}", if mi.units.inch() { "Inch" } else { "Metric" }, mi.axes.iter().map(|a| a.letter).collect::<String>()),
        )),
    }
    let limits = ["X", "Y", "Z"].iter().filter(|a| mi.has_axis(a.chars().next().unwrap())).all(|a| {
        let s = format!("AXIS_{a}");
        cfg.get_f64(&s, "MIN_LIMIT").is_some() && cfg.get_f64(&s, "MAX_LIMIT").is_some()
    });
    if limits {
        v.push(Check::new("machine.limits", G::Machine, L::Ok, "Soft limits set: programs are checked against travel"));
    } else {
        v.push(
            Check::new("machine.limits", G::Machine, L::Attention, "Soft limits missing: no travel check before a program runs")
                .detail("Set MIN_LIMIT and MAX_LIMIT in [AXIS_X], [AXIS_Y] and [AXIS_Z]."),
        );
    }
    let ztop = format!("G53 Z{} {}", mi.units.fmt(mi.z_top), mi.units.name());
    v.push(
        Check::new("machine.z-top", G::Machine, L::Info, format!("Retracts go to {ztop}"))
            .detail(if mc.machine.z_top.is_some() { "From [machine]z_top." } else { "Default: just under [AXIS_Z]MAX_LIMIT." })
            .fix(Fix::Setting("machine.z_top")),
    );
    if !mi.trivial_kins {
        v.push(Check::new("machine.kins", G::Machine, L::Info, "Not trivkins: HOME ALL only, and nothing jogs before homing"));
    } else if mi.axes.iter().any(|a| a.joints.len() > 1) {
        v.push(Check::new("machine.gantry", G::Machine, L::Info, "Gantry: an axis with two joints jogs only after homing"));
    }
    if !mi.home_all {
        v.push(
            Check::new("machine.home-sequence", G::Machine, L::Info, "HOME ALL is off: not every joint has a HOME_SEQUENCE")
                .detail("Home each axis on its own, or set HOME_SEQUENCE in every [JOINT_n]."),
        );
    }
    let (_, bad) = crate::machine_info::jog_steps(cfg.get("DISPLAY", "INCREMENTS"), mi.units);
    if !bad.is_empty() {
        v.push(
            Check::new("machine.increments", G::Machine, L::Attention, format!("Jog steps ferrum cannot read: {}", bad.join(", ")))
                .detail("They are left out. [DISPLAY]INCREMENTS takes numbers with an optional unit: 1mm, .001in, 1/16 in, 10 mil."),
        );
    }

    // Files
    let prefix = crate::files::root(cfg);
    if prefix.is_dir() {
        v.push(Check::new("files.programs", G::Files, L::Ok, format!("Programs in {}", prefix.display())));
    } else {
        v.push(
            Check::new("files.programs", G::Files, L::Attention, format!("Program folder {} does not exist", prefix.display()))
                .fix(Fix::IniLines { section: "DISPLAY", lines: vec!["PROGRAM_PREFIX = ~/linuxcnc/nc_files".into()] }),
        );
    }
    let tbl = resolve(cfg, cfg.get("EMCIO", "TOOL_TABLE").unwrap_or("tool.tbl"));
    if cfg.get("EMCIO", "DB_PROGRAM").is_some() {
        v.push(Check::new("files.tool-db", G::Files, L::Attention, "[EMCIO]DB_PROGRAM: ferrum edits tool.tbl and does not know the tool database"));
    } else if !tbl.is_file() {
        v.push(Check::new("files.tool-table", G::Files, L::Attention, format!("Tool table {} not found", tbl.display())));
    }
    let var = resolve(cfg, cfg.get("RS274NGC", "PARAMETER_FILE").unwrap_or("linuxcnc.var"));
    if !var.is_file() {
        v.push(
            Check::new("files.parameter-file", G::Files, L::Info, format!("Parameter file {} not found", var.display()))
                .detail("OFFSETS shows zeros for work systems that are not active until LinuxCNC writes it."),
        );
    }
    if cfg!(target_os = "wasi") {
        v.push(Check::new("files.rs274", G::Files, L::Ok, "Browser demo: preview and travel check from ferrum's own G-code reader"));
    } else if preview::rs274::available() {
        v.push(Check::new("files.rs274", G::Files, L::Ok, "rs274 found: preview and travel check on"));
    } else {
        v.push(
            Check::new("files.rs274", G::Files, L::Blocks(vec![Feature::Preview]), "rs274 is not on PATH: no preview, no travel check")
                .detail("It comes with LinuxCNC; start ferrum from LinuxCNC so PATH includes it."),
        );
    }

    // Spindle
    match mi.max_rpm {
        Some(m) => v.push(Check::new("spindle.max", G::Spindle, L::Ok, format!("Top speed {m:.0} rpm{}", if mi.reverse { "" } else { ", no reverse" }))),
        None => v.push(
            Check::new("spindle.max", G::Spindle, L::Attention, "Spindle top speed unknown: speeds are not checked")
                .fix(Fix::Setting("spindle.max_rpm")),
        ),
    }

    // Tool change
    let (style, why) = toolchange_style(mc, i.modules, i.hal);
    let what = match style {
        ToolChangeStyle::External => "M6 is answered by the machine; ferrum shows the tool",
        ToolChangeStyle::Dialog => "M6 is answered by ferrum's tool change dialog",
        ToolChangeStyle::Drawbar => "M6 is answered by ferrum's dialog with the power drawbar",
    };
    v.push(Check::new("toolchange.style", G::ToolChange, L::Info, what).detail(why).fix(Fix::Setting("toolchange.style")));
    if let Some(Some(sig)) = i.hal.pin_signal("iocontrol.0.tool-prepared") {
        if sig.is_empty() {
            v.push(
                Check::new("toolchange.prepare", G::ToolChange, L::Attention, "iocontrol.0.tool-prepared is not connected: a T word waits forever")
                    .fix(Fix::Hal("net tool-prep-loop iocontrol.0.tool-prepare => iocontrol.0.tool-prepared".into())),
            );
        }
    }

    // Probe
    let probe_on = i.modules.iter().any(|m| m == "probe");
    let pdefs = crate::probe_defaults::resolve(mc, &crate::probe_defaults::param_file(cfg), mi.units.inch());
    if probe_on {
        let tool = mc.probe_tool(cfg);
        let src = if mc.probe.tool.is_some() {
            "[probe]tool"
        } else if cfg.get("PROBE", "TOOL_NUMBER").is_some() {
            "[PROBE]TOOL_NUMBER"
        } else {
            "ferrum's default"
        };
        v.push(Check::new("probe.tool", G::Probe, L::Info, format!("The probe is T{tool}")).detail(format!("From {src}.")).fix(Fix::Setting("probe.tool")));
        v.extend(defaults_row(&pdefs, false));
        v.push(macros_row(cfg, &macros_dir()));
        match i.hal.pin_signal("motion.probe-input") {
            Some(Some(s)) if s.is_empty() => v.push(
                Check::new("probe.input", G::Probe, L::Blocks(vec![Feature::Probe]), "motion.probe-input is not connected: a probe move would never stop")
                    .detail("motion.probe-input is not connected in HAL, so a probe move would never stop. Connect your probe's input to it.")
                    .fix(Fix::Hal("net probe-in <your probe's input pin> => motion.probe-input".into())),
            ),
            Some(Some(s)) => v.push(Check::new("probe.input", G::Probe, L::Ok, format!("Probe input on signal {s}"))),
            _ => {}
        }
    }

    // Tool setter: touch-off goes to G30 and probes down.
    if mc.toolsetter.enabled.unwrap_or(i.modules.iter().any(|m| m == "util")) {
        v.extend(defaults_row(&pdefs, true));
        let g30 = std::fs::read_to_string(&var).ok().map(|t| {
            t.lines()
                .filter_map(|l| {
                    let mut w = l.split_whitespace();
                    Some((w.next()?.parse::<u32>().ok()?, w.next()?.parse::<f64>().ok()?))
                })
                .filter(|(n, _)| (5181..=5183).contains(n))
                .any(|(_, v)| v != 0.0)
        });
        if g30 == Some(true) {
            v.push(Check::new("toolsetter.g30", G::ToolSetter, L::Ok, "Tool setter position (G30) is set"));
        } else {
            v.push(
                Check::new("toolsetter.g30", G::ToolSetter, L::Attention, "Tool setter position (G30) is not set: touch-off would probe at machine zero")
                    .detail("Jog over the setter and use UTIL › SET G30 HERE."),
            );
        }
    }

    // Job and conversational: the programs ferrum writes read HAL pins.
    let jobs = i.modules.iter().any(|m| m == "job" || m == "conv");
    if jobs {
        let mut lines = Vec::new();
        if !on(cfg.get("RS274NGC", "HAL_PIN_VARS")) {
            lines.push("HAL_PIN_VARS = 1".to_string());
        }
        if !on(cfg.get("RS274NGC", "INI_VARS")) && cfg.get("RS274NGC", "INI_VARS").is_some() {
            lines.push("INI_VARS = 1".to_string());
        }
        if lines.is_empty() {
            v.push(Check::new("job.interp", G::Job, L::Ok, "Interpreter reads HAL pins (HAL_PIN_VARS)"));
        } else {
            v.push(
                Check::new("job.interp", G::Job, L::Blocks(vec![Feature::Job]), "Jobs need HAL_PIN_VARS = 1 in [RS274NGC]")
                    .detail("JOB needs HAL_PIN_VARS = 1 in [RS274NGC]: a built job checks its stamp through #<_hal[...]>, and without it every job stops at its first line.")
                    .fix(Fix::IniLines { section: "RS274NGC", lines }),
            );
        }
    }

    // Display
    if let Some(Ok(port)) = mc.remote_port(cfg) {
        v.push(Check::new("display.remote", G::Display, L::Attention, format!("Remote screen on port {port}: anyone who can reach it can run the machine")));
    }
    if let Some(f) = cfg.get("HAL", "POSTGUI_HALFILE") {
        v.push(
            Check::new("hal.postgui", G::Hal, L::Info, format!("[HAL]POSTGUI_HALFILE ({f}) is not run by ferrum"))
                .detail("It was written for another screen's pins. List ferrum's own in [display]postgui."),
        );
    }
    if i.hal.pin_signal("motion.probe-input").is_none() {
        v.push(Check::new("hal.up", G::Hal, L::Info, "LinuxCNC is not running: HAL wiring was not checked"));
    }
    v
}

/// ferrum's probe macros (ferrum/macros): `FERRUM_MACROS`, else a
/// `macros` folder beside the binary or up to three folders above it (a
/// source checkout's target/release/ferrum), else /usr/share/ferrum/macros.
pub fn macros_dir() -> Option<PathBuf> {
    let has = |d: &Path| d.join("ferrum_touch.ngc").is_file();
    if let Some(d) = std::env::var_os("FERRUM_MACROS").map(PathBuf::from) {
        return has(&d).then_some(d);
    }
    let exe = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    if let Some(exe) = exe {
        for dir in exe.ancestors().skip(1).take(4) {
            for d in [dir.join("macros"), dir.join("share/ferrum/macros")] {
                if has(&d) {
                    return Some(d);
                }
            }
        }
    }
    Some(PathBuf::from("/usr/share/ferrum/macros")).filter(|d| has(d))
}

/// Where PROBE's macros come from. `ours` is ferrum's macro folder.
fn macros_row(cfg: &Config, ours: &Option<PathBuf>) -> Check {
    let dirs = subroutine_dirs(cfg);
    let found = |n: &str| dirs.iter().find(|d| d.join(format!("{n}.ngc")).is_file()).cloned();
    let missing = missing_subs(cfg, crate::probe::MACROS);
    let same = |a: &Path, b: &Path| a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b);
    let ours_first = ours.as_ref().is_some_and(|o| found("ferrum_touch").is_some_and(|d| same(&d, o)));
    // The INI line that puts ferrum's folder first.
    let fix = |o: &Path| {
        let rest = cfg.get("RS274NGC", "SUBROUTINE_PATH").map(str::trim).filter(|r| !r.is_empty());
        Fix::IniLines {
            section: "RS274NGC",
            lines: vec![format!("SUBROUTINE_PATH = {}{}", o.display(), rest.map(|r| format!(":{r}")).unwrap_or_default())],
        }
    };
    if missing.len() == crate::probe::MACROS.len() {
        let c = Check::new("probe.macros", Group::Probe, Level::Blocks(vec![Feature::Probe]), "The probe macros are not on SUBROUTINE_PATH");
        return match ours {
            Some(o) => c.detail(format!("ferrum's probe macros are in {}. Put that folder first on [RS274NGC]SUBROUTINE_PATH and restart LinuxCNC.", o.display())).fix(fix(o)),
            None => c.detail("PROBE runs ferrum's probe macros (ferrum/macros), and they were not found. Put that folder first on [RS274NGC]SUBROUTINE_PATH and restart LinuxCNC."),
        };
    }
    if !missing.is_empty() {
        return Check::new("probe.macros", Group::Probe, Level::Attention, format!("{} probe macros not found", missing.len()))
            .detail(format!("Routines that call them stop with a message: {}.", missing.join(", ")));
    }
    if ours_first {
        return Check::new("probe.macros", Group::Probe, Level::Ok, "ferrum's probe macros found");
    }
    // Another set (Probe Basic's, the machine's own) answers first.
    let from = found("probe_x_plus_wco").map(|d| d.display().to_string()).unwrap_or_default();
    let c = Check::new("probe.macros", Group::Probe, Level::Info, format!("Probe macros from {from}"))
        .detail("Not ferrum's own: PROBE expects their results in ferrum's format (ferrum/macros/SPEC.md).");
    match ours {
        Some(o) => c.fix(fix(o)),
        None => c,
    }
}

/// The probe's (or the tool setter's) stored values: missing ones stop the
/// macros; starting values want checking.
fn defaults_row(r: &std::collections::BTreeMap<u16, (Option<f64>, crate::probe_defaults::Source)>, setter: bool) -> Option<Check> {
    use crate::probe_defaults::{titles, Source};
    let (group, id, what, step) =
        if setter { (Group::ToolSetter, "toolsetter.values", "Tool setter", "Tool setter") } else { (Group::Probe, "probe.values", "Probe", "Probe") };
    // The calibration block widths matter only to that calibration.
    let missing: Vec<&str> = titles(r, setter, Source::Missing).into_iter().filter(|t| setter || !t.starts_with("Calibration block")).collect();
    let seeded = titles(r, setter, Source::Seed);
    let list = |v: &[&str]| v.join(", ");
    if !missing.is_empty() {
        return Some(
            Check::new(id, group, Level::Attention, format!("{what} values not set: {}", list(&missing)))
                .detail(format!("Routines that need them stop at their first line. Set them in SETUP › {step}, where they are listed after the checks.")),
        );
    }
    if !seeded.is_empty() {
        let cal = r.get(&3032).is_some_and(|(_, s)| *s == Source::Seed);
        return Some(
            Check::new(id, group, Level::Attention, format!("{what} feeds and distances are starting values"))
                .detail(format!(
                    "{} for a 2 mm tip probe, not checked on this machine. Check them in SETUP › {step}, where they are listed after the checks; each one changed is saved to ferrum.toml.{}",
                    if seeded.len() > 3 { format!("{} values are ferrum's starting values", seeded.len()) } else { format!("Starting values: {}", list(&seeded)) },
                    if cal { " The probe is not calibrated: run CAL › ring gauge." } else { "" }
                )),
        );
    }
    let from_file = titles(r, setter, Source::ParamFile).len();
    Some(Check::new(id, group, Level::Ok, if from_file > 0 { format!("{what} values set ({from_file} from the parameter file)") } else { format!("{what} values set") }))
}

/// `text` (an INI) with `lines` ("KEY = VALUE") in `[section]`: a key
/// already there has its line replaced, the rest go at the end of the
/// section (before its trailing blank lines), and a missing section is
/// added at the end of the file.
pub fn ini_with(text: &str, section: &str, lines: &[String]) -> String {
    let key = |l: &str| l.split_once('=').map(|(k, _)| k.trim().to_string());
    let mut out: Vec<String> = text.lines().map(String::from).collect();
    let head = out.iter().position(|l| l.trim().eq_ignore_ascii_case(&format!("[{section}]")));
    let Some(head) = head else {
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(format!("[{section}]"));
        out.extend(lines.iter().cloned());
        return out.join("\n") + "\n";
    };
    let end = out[head + 1..].iter().position(|l| l.trim_start().starts_with('[')).map_or(out.len(), |i| head + 1 + i);
    let mut add = Vec::new();
    for line in lines {
        let k = key(line);
        match out[head + 1..end].iter().position(|l| !l.trim_start().starts_with('#') && key(l) == k) {
            Some(i) => out[head + 1 + i] = line.clone(),
            None => add.push(line.clone()),
        }
    }
    // After the section's last non-blank line.
    let mut at = end;
    while at > head + 1 && out[at - 1].trim().is_empty() {
        at -= 1;
    }
    for (n, l) in add.into_iter().enumerate() {
        out.insert(at + n, l);
    }
    out.join("\n") + "\n"
}

/// `ferrum --check --write-ini`: put every row's INI lines into the INI,
/// keeping the old file as `<ini>.bak`. Returns what was written.
pub fn write_ini(path: &Path, checks: &[Check]) -> Result<Vec<String>, String> {
    let fixes: Vec<(&str, &Vec<String>)> = checks
        .iter()
        .filter(|c| matches!(c.level, Level::Attention | Level::Blocks(_)))
        .filter_map(|c| match &c.fix {
            Fix::IniLines { section, lines } => Some((*section, lines)),
            _ => None,
        })
        .collect();
    if fixes.is_empty() {
        return Ok(Vec::new());
    }
    let mut text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut done = Vec::new();
    for (section, lines) in fixes {
        text = ini_with(&text, section, lines);
        done.extend(lines.iter().map(|l| format!("[{section}] {l}")));
    }
    let bak = path.with_extension(format!("{}.bak", path.extension().and_then(|e| e.to_str()).unwrap_or("ini")));
    std::fs::copy(path, &bak).map_err(|e| format!("{}: {e}", bak.display()))?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(done)
}

/// The reason a feature is off, from the first row that blocks it: its
/// detail (written to stand alone), else its title.
pub fn blocked(checks: &[Check], f: Feature) -> Option<String> {
    checks.iter().find(|c| c.blocks(f)).map(|c| if c.detail.is_empty() { c.title.clone() } else { c.detail.clone() })
}

/// The rows as text, grouped, for `ferrum --check`.
pub fn report(checks: &[Check]) -> String {
    let mut groups: Vec<Group> = checks.iter().map(|c| c.group).collect();
    groups.sort();
    groups.dedup();
    let mut s = String::new();
    for g in groups {
        s += &format!("{}\n", g.name());
        for c in checks.iter().filter(|c| c.group == g) {
            let mark = match &c.level {
                Level::Ok => "  ok   ",
                Level::Info => "  info ",
                Level::Attention => "  ATTN ",
                Level::Blocks(_) => "  OFF  ",
            };
            s += &format!("{mark}{}\n", c.title);
            if !c.detail.is_empty() {
                s += &format!("         {}\n", c.detail);
            }
            match &c.fix {
                Fix::IniLines { section, lines } => {
                    s += &format!("         fix: in [{section}] add: {} (then restart LinuxCNC)\n", lines.join("; "));
                }
                Fix::Hal(h) => s += &format!("         fix: in your HAL: {h}\n"),
                Fix::Setting(_) | Fix::None => {}
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_lines_go_into_their_section() {
        let ini = "[RS274NGC]\nPARAMETER_FILE = sim.var\nHAL_PIN_VARS = 0\n\n[TRAJ]\nLINEAR_UNITS = mm\n";
        let out = ini_with(ini, "RS274NGC", &["HAL_PIN_VARS = 1".into(), "INI_VARS = 1".into()]);
        assert_eq!(out, "[RS274NGC]\nPARAMETER_FILE = sim.var\nHAL_PIN_VARS = 1\nINI_VARS = 1\n\n[TRAJ]\nLINEAR_UNITS = mm\n");
        let out = ini_with("[EMC]\nVERSION = 1.1\n", "RS274NGC", &["HAL_PIN_VARS = 1".into()]);
        assert_eq!(out, "[EMC]\nVERSION = 1.1\n\n[RS274NGC]\nHAL_PIN_VARS = 1\n");
        // A commented-out key is left alone.
        let out = ini_with("[RS274NGC]\n# HAL_PIN_VARS = 0\n", "RS274NGC", &["HAL_PIN_VARS = 1".into()]);
        assert_eq!(out, "[RS274NGC]\n# HAL_PIN_VARS = 0\nHAL_PIN_VARS = 1\n");
    }
    use std::collections::HashMap;

    struct FakeHal {
        pins: HashMap<&'static str, &'static str>,
        writers: HashMap<&'static str, &'static str>,
    }

    impl Hal for FakeHal {
        fn pin_signal(&self, pin: &str) -> Option<Option<String>> {
            Some(self.pins.get(pin).map(|s| s.to_string()))
        }
        fn signal_writer(&self, sig: &str) -> Option<String> {
            self.writers.get(sig).map(|s| s.to_string())
        }
    }

    #[test]
    fn halcmd_output() {
        let pin = "motmod bit   IN FALSE motion.probe-input <== probe-in\n";
        assert_eq!(parse_pin(pin, "motion.probe-input"), Some("probe-in".into()));
        assert_eq!(parse_pin("iocontrol bit   IN FALSE iocontrol.0.tool-prepared\n", "iocontrol.0.tool-prepared"), Some(String::new()));
        assert_eq!(parse_pin("", "x.y"), None);
        let sig = "bit    FALSE  tool-change-confirmed ==> iocontrol.0.tool-changed <== ferrum.drawbar.tool-changed\n";
        assert_eq!(parse_writer(sig, "tool-change-confirmed"), Some("ferrum.drawbar.tool-changed".into()));
        assert_eq!(parse_writer("bit FALSE probe-in ==> motion.probe-input\n", "probe-in"), None);
    }

    #[test]
    fn toolchange_style_is_detected() {
        let mc = MachineCfg::default();
        let looped = FakeHal { pins: [("iocontrol.0.tool-changed", "tool-change-loop")].into(), writers: [("tool-change-loop", "iocontrol.0.tool-change")].into() };
        assert_eq!(toolchange_style(&mc, &[], &looped).0, ToolChangeStyle::External);
        let free = FakeHal { pins: [("iocontrol.0.tool-changed", "")].into(), writers: HashMap::new() };
        assert_eq!(toolchange_style(&mc, &[], &free).0, ToolChangeStyle::Dialog);
        assert_eq!(toolchange_style(&mc, &["drawbar".into()], &looped).0, ToolChangeStyle::Drawbar);
        assert_eq!(toolchange_style(&mc, &[], &NoHal).0, ToolChangeStyle::Dialog);
    }

    #[test]
    fn a_stock_inch_machine() {
        let mut cfg = Config::default();
        cfg.path = Some("/nonexistent/stock.ini".into());
        cfg.insert("TRAJ", "LINEAR_UNITS", "inch");
        cfg.insert("DISPLAY", "INCREMENTS", ".1in, 1/16 in, banana");
        cfg.insert("RS274NGC", "HAL_PIN_VARS", "1");
        let mi = MachineInfo::from_ini(&cfg);
        let mc = MachineCfg::default();
        let modules: Vec<String> = "run probe job".split(' ').map(String::from).collect();
        let hal = FakeHal { pins: [("motion.probe-input", "")].into(), writers: HashMap::new() };
        let c = run(&Inputs { cfg: &cfg, mc: &mc, mi: &mi, notes: &[], modules: &modules, hal: &hal });
        let ids: Vec<&str> = c.iter().map(|c| c.id).collect();
        assert!(ids.contains(&"machine.increments"), "{ids:?}");
        // PROBE, JOB and CONV work in inches: only the missing probe input
        // stops PROBE here.
        assert!(!ids.iter().any(|id| id.ends_with(".inch")), "{ids:?}");
        assert!(blocked(&c, Feature::Job).is_none());
        assert!(blocked(&c, Feature::Conv).is_none());
        assert!(blocked(&c, Feature::Core).is_none());
        let r = c.iter().find(|c| c.id == "machine.increments").unwrap();
        assert!(r.title.contains("banana") && !r.title.contains("1/16"), "{}", r.title);
        assert!(c.iter().any(|c| c.id == "probe.input" && c.blocks(Feature::Probe)));
        assert!(!report(&c).contains("inch machines"));
        // No parameter file and no ferrum.toml values: starting values.
        let r = c.iter().find(|c| c.id == "probe.values").unwrap();
        assert!(r.level == Level::Attention && r.title.contains("starting values"), "{r:?}");
    }

    #[test]
    fn setter_values_without_a_default_must_be_set() {
        let mut cfg = Config::default();
        cfg.path = Some("/nonexistent/stock.ini".into());
        let mi = MachineInfo::from_ini(&cfg);
        let modules: Vec<String> = vec!["util".into()];
        let (mc, _) = MachineCfg::parse("[toolsetter]\nmax_z = 100\n");
        let c = run(&Inputs { cfg: &cfg, mc: &mc, mi: &mi, notes: &[], modules: &modules, hal: &NoHal });
        let r = c.iter().find(|c| c.id == "toolsetter.values").unwrap();
        assert_eq!(r.title, "Tool setter values not set: Reference height (spindle zero)");
        let (mc, _) = MachineCfg::parse("[toolsetter]\nmax_z = 100\nreference_height = 250\nfast_feed = 1\nslow_feed = 1\ntraverse_feed = 1\nretract = 1\nbreakage_tolerance = 1\n");
        let c = run(&Inputs { cfg: &cfg, mc: &mc, mi: &mi, notes: &[], modules: &modules, hal: &NoHal });
        assert_eq!(c.iter().find(|c| c.id == "toolsetter.values").unwrap().level, Level::Ok);
    }

    #[test]
    fn no_ini_blocks_core() {
        let c = run(&Inputs { cfg: &Config::default(), mc: &MachineCfg::default(), mi: &MachineInfo::default(), notes: &[], modules: &[], hal: &NoHal });
        assert!(blocked(&c, Feature::Core).is_some());
    }
}
