//! PROBE screen (milestone 5): one screen for every probing routine.
//!
//! Pick FIND / MEASURE / CAL, the shape, then the spot on the picture; the
//! spot decides which existing macro runs (Probe Basic's vendor macros and
//! the guarded mymill ones). Nothing here moves the machine itself.
//!
//! Correctness rules (HANDOFF.md, REQUIREMENTS.md M5):
//! - Per-run values: the macros read #3004–#3037. A changed value is written
//!   before the macro and the saved default written back afterwards, after a
//!   success, a failure or STOP alike. A state file marks a run in progress
//!   so a crash mid-run is repaired at the next start (`restore_pending`).
//! - Defaults are the parameter file's values at startup, unless that marker
//!   says an earlier run never restored them; then the saved copy wins.
//!   Calibration macros change #3010/#3032 themselves; their reported value
//!   becomes the new default so the restore keeps it.
//! - Results arrive as the macros' `(DEBUG, EVAL[vcp.getWidget{..}...])`
//!   lines; they are shown, never trusted to have written anything.
//! - FIND writes to the chosen WCS by making it active for the run and
//!   switching back afterwards.

use crate::config::Config;
use crate::manual::KeypadTarget;
use crate::settings::Settings;
use crate::toolchange::Io;
use crate::{Actions, Live, MainWindow, ProbeItem, ProbeParam, ProbeSpot};
use linuxcnc::{Command, Handle, Message, MessageKind, Status};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const WCS: [&str; 9] = ["G54", "G55", "G56", "G57", "G58", "G59", "G59.1", "G59.2", "G59.3"];

struct Param {
    n: u16,
    label: &'static str,
    unit: &'static str,
    seg: &'static [&'static str],
    group: &'static str,
}

const fn p(n: u16, label: &'static str, unit: &'static str, group: &'static str) -> Param {
    Param { n, label, unit, seg: &[], group }
}
const fn s(n: u16, label: &'static str, seg: &'static [&'static str], group: &'static str) -> Param {
    Param { n, label, unit: "", seg, group }
}

/// #3004–#3037 as Probe Basic's settings tabs name them.
const PARAMS: &[Param] = &[
    p(3004, "Fast feed", "mm/min", "setter"),
    p(3005, "Slow feed", "mm/min", "setter"),
    p(3006, "Traverse feed", "mm/min", "setter"),
    p(3007, "Max Z travel", "mm", "setter"),
    p(3008, "Max XY travel", "mm", "setter"),
    p(3009, "Retract distance", "mm", "setter"),
    p(3010, "Spindle zero height", "mm", "setter"),
    s(3011, "Diameter probe mode", &["OFF", "ON"], "setter"),
    s(3012, "Diameter offset mode", &["OFF", "ON"], "setter"),
    s(3013, "Offset direction", &["−", "+"], "setter"),
    p(3014, "Probe tool number", "", "probe"),
    p(3015, "Slow feed", "mm/min", "probe"),
    p(3016, "Fast feed", "mm/min", "probe"),
    p(3017, "Traverse feed", "mm/min", "probe"),
    p(3018, "Max XY distance", "mm", "probe"),
    p(3019, "XY clearance", "mm", "probe"),
    p(3020, "Max Z distance", "mm", "probe"),
    p(3021, "Z clearance", "mm", "probe"),
    p(3022, "Extra probe depth", "mm", "probe"),
    p(3023, "Step off width", "mm", "probe"),
    p(3024, "Edge width", "mm", "probe"),
    p(3025, "Diameter hint", "mm", "probe"),
    p(3026, "Boss/pocket X hint", "mm", "probe"),
    p(3027, "Boss/pocket Y hint", "mm", "probe"),
    p(3028, "Ridge/valley X hint", "mm", "probe"),
    p(3029, "Ridge/valley Y hint", "mm", "probe"),
    s(3030, "Result mode", &["WRITE WCO", "POSITION ONLY"], "probe"),
    s(3031, "WCO rotation", &["OFF", "ON"], "probe"),
    p(3032, "Calibration offset", "mm", "cal"),
    p(3033, "Ring gauge diameter", "mm", "cal"),
    p(3034, "Cal block X width", "mm", "cal"),
    p(3035, "Cal block Y width", "mm", "cal"),
    s(3036, "Cal axis", &["X", "Y", "X+Y"], "cal"),
    p(3037, "Breakage tolerance", "mm", "cal"),
];

fn param(n: u16) -> Option<&'static Param> {
    PARAMS.iter().find(|p| p.n == n)
}

fn step_of(n: u16) -> f64 {
    let p = param(n);
    if p.is_some_and(|p| !p.seg.is_empty()) {
        1.0
    } else if p.is_some_and(|p| p.unit.contains("/min")) {
        10.0
    } else if [3019, 3021, 3009, 3022].contains(&n) {
        0.5
    } else if n == 3033 || n == 3032 {
        0.1
    } else {
        1.0
    }
}

fn fmt_value(n: u16, v: f64) -> String {
    if let Some(p) = param(n).filter(|p| !p.seg.is_empty()) {
        return p.seg.get(v as usize).map_or_else(|| format!("{v}"), |s| s.to_string());
    }
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".into() } else { s.into() }
}

/// What a target needs in the spindle.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Needs {
    Probe,
    /// A cutting tool (not the probe) for the tool setter.
    Tool,
    /// The empty spindle, for the nose reference.
    Empty,
}

#[derive(Clone, Debug)]
struct Target {
    id: &'static str,
    x: f64,
    y: f64,
    /// Edge hotspots are rectangles; zero for a round spot.
    w: f64,
    h: f64,
    label: String,
    kind: &'static str,
}

fn corner(id: &'static str, x: f64, y: f64, label: &str) -> Target {
    Target { id, x, y, w: 0.0, h: 0.0, label: label.into(), kind: "corner" }
}
fn edge(id: &'static str, x: f64, y: f64, w: f64, h: f64, label: &str) -> Target {
    Target { id, x, y, w, h, label: label.into(), kind: "edge" }
}
fn center(kind: &'static str, x: f64, y: f64, label: &str) -> Target {
    Target { id: "ctr", x, y, w: 0.0, h: 0.0, label: label.into(), kind }
}

const SHAPES: &[(&str, &[(&str, &str)])] = &[
    ("find", &[("block", "BLOCK / STOCK"), ("round", "ROUND BOSS"), ("pocket", "POCKET"), ("bore", "BORE"), ("slot", "RIDGE / SLOT"), ("cyl", "ON THE ROTARY")]),
    ("meas", &[("dia", "DIAMETER"), ("runout", "RUNOUT"), ("defl", "DEFLECTION")]),
    ("cal", &[("ring", "PROBE TIP"), ("setter", "TOOL LENGTH"), ("nose", "SPINDLE NOSE")]),
];

fn side_name(c: char) -> &'static str {
    match c {
        'F' => "FRONT",
        'B' => "BACK",
        'L' => "LEFT",
        _ => "RIGHT",
    }
}
fn side_axis(c: char) -> &'static str {
    if c == 'L' || c == 'R' { "X" } else { "Y" }
}

/// Operator-only inputs that are macro arguments, not #30xx parameters.
#[derive(Clone, Copy)]
struct Local {
    label: &'static str,
    default: f64,
}
fn locals(kind: &str) -> &'static [Local] {
    match kind {
        "stock" => &[Local { label: "Max span", default: 150.0 }],
        "cyl" => &[Local { label: "Reference dia est.", default: 30.0 }],
        "dia" | "runout" => &[Local { label: "Diameter est.", default: 25.0 }],
        "defl" => &[Local { label: "Diameter est.", default: 25.0 }, Local { label: "X span", default: 50.0 }],
        _ => &[],
    }
}

/// Multi-step routines: Auto Stock (measure, then apply) and Two Faces
/// (first face, opposite face, apply).
#[derive(Clone, Debug, PartialEq)]
enum Flow {
    StockMeasured { token: u32, center: (f64, f64), size: (f64, f64) },
    TwoFirst { token: u32, axis: u8, dir: i32 },
    TwoReady { token: u32, width: f64 },
}

#[derive(Debug)]
struct Run {
    /// MDI lines still to send; the restore lines run whatever happens.
    steps: Vec<String>,
    restore: Vec<String>,
    sent: Option<(Instant, i32, bool)>,
    failed: Option<String>,
    restoring: bool,
    what: String,
    kind: &'static str,
    flow_step: Option<&'static str>,
}

pub struct Probe {
    mode: &'static str,
    shape: &'static str,
    tgt: &'static str,
    axes: &'static str,
    angle: bool,
    start: char,
    both: bool,
    ridge: bool,
    wcs: Option<usize>,
    runv: BTreeMap<u16, f64>,
    locv: BTreeMap<usize, f64>,
    defaults: BTreeMap<u16, f64>,
    results: Vec<(String, Vec<f64>)>,
    result_text: String,
    flow: Option<Flow>,
    token: u32,
    run: Option<Run>,
    status: Option<Status>,
    state: Settings,
    restore_pending: bool,
    probe_tool: i32,
    machine: Handle,
    io: Option<Rc<dyn Io>>,
    ui: slint::Weak<MainWindow>,
    rotation: Box<dyn Fn(usize) -> f64>,
}

impl Probe {
    fn pv(&self, n: u16) -> f64 {
        self.runv.get(&n).or_else(|| self.defaults.get(&n)).copied().unwrap_or(0.0)
    }

    fn targets(&self) -> Vec<Target> {
        match self.shape {
            "block" => {
                let ctr = if self.axes == "XY" { "STOCK CENTER".to_string() } else { format!("CENTER IN {}", self.axes) };
                vec![
                    corner("c-bl", 100.0, 70.0, "BACK LEFT CORNER"),
                    corner("c-br", 300.0, 70.0, "BACK RIGHT CORNER"),
                    corner("c-fl", 100.0, 230.0, "FRONT LEFT CORNER"),
                    corner("c-fr", 300.0, 230.0, "FRONT RIGHT CORNER"),
                    edge("e-b", 140.0, 58.0, 120.0, 24.0, "BACK EDGE"),
                    edge("e-f", 140.0, 218.0, 120.0, 24.0, "FRONT EDGE"),
                    edge("e-l", 88.0, 105.0, 24.0, 90.0, "LEFT EDGE"),
                    edge("e-r", 288.0, 105.0, 24.0, 90.0, "RIGHT EDGE"),
                    center(if self.axes == "XY" { "stock" } else { "two" }, 200.0, 150.0, &format!("{ctr} · FROM {}", side_name(self.start))),
                ]
            }
            "pocket" => vec![
                Target { kind: "icorner", ..corner("i-bl", 110.0, 80.0, "BACK LEFT INSIDE CORNER") },
                Target { kind: "icorner", ..corner("i-br", 290.0, 80.0, "BACK RIGHT INSIDE CORNER") },
                Target { kind: "icorner", ..corner("i-fl", 110.0, 220.0, "FRONT LEFT INSIDE CORNER") },
                Target { kind: "icorner", ..corner("i-fr", 290.0, 220.0, "FRONT RIGHT INSIDE CORNER") },
                center("pocket", 200.0, 150.0, "POCKET CENTER"),
            ],
            "round" => vec![center("boss", 200.0, 150.0, "BOSS CENTER")],
            "bore" => vec![center("bore", 200.0, 150.0, "BORE CENTER")],
            "slot" => {
                let what = if self.ridge { "RIDGE" } else { "SLOT" };
                vec![center("slot", 200.0, 150.0, &format!("{what} · ACROSS {}", if self.axes == "Y" { "Y" } else { "X" }))]
            }
            "cyl" => vec![center("cyl", 200.0, 165.0, "ROTARY CENTERLINE (Y, Z)"), Target { id: "flat", ..center("flat", 200.0, 115.0, "A0 ON A FLAT") }],
            "dia" => vec![center("dia", 200.0, 150.0, "DIAMETER")],
            "runout" => vec![center("runout", 200.0, 150.0, "RUNOUT")],
            "defl" => vec![center("defl", 200.0, 150.0, "DEFLECTION")],
            "ring" => vec![center("ring", 200.0, 150.0, "PROBE TIP")],
            "setter" => vec![center("setter", 200.0, 150.0, "TOOL LENGTH")],
            _ => vec![center("nose", 200.0, 150.0, "SPINDLE NOSE")],
        }
    }

    fn selected(&self) -> Target {
        let t = self.targets();
        t.iter().find(|t| t.id == self.tgt).cloned().unwrap_or_else(|| t[0].clone())
    }

    /// The two to four values a routine depends on; the rest are under MORE.
    fn key_params(&self, kind: &str) -> Vec<u16> {
        match kind {
            "corner" | "icorner" => vec![3023, 3019, 3018],
            "edge" if self.angle => vec![3024, 3023, 3019],
            "edge" => vec![3019, 3018],
            "stock" => vec![3019, 3018],
            "two" => vec![3019, 3018],
            "boss" => vec![3025, 3019, 3018],
            "pocket" => vec![3026, 3027, 3018],
            "bore" => vec![3025, 3018],
            "slot" => vec![if self.axes == "Y" { 3029 } else { 3028 }, 3019],
            "cyl" | "flat" | "dia" | "runout" | "defl" => vec![3021, 3019],
            "setter" | "nose" => vec![3005, 3009],
            "ring" => vec![3033],
            _ => vec![],
        }
    }

    fn used_params(&self, kind: &str) -> Vec<u16> {
        match kind {
            "cyl" | "flat" | "dia" | "runout" | "defl" => vec![3019, 3021, 3016, 3015],
            "setter" | "nose" => vec![3004, 3005, 3006, 3007, 3009],
            "ring" => vec![3033, 3016, 3015, 3017, 3023, 3020, 3021],
            _ => vec![3023, 3019, 3021, 3018, 3020, 3022, 3016, 3015, 3017],
        }
    }

    fn needs(kind: &str) -> Needs {
        match kind {
            "setter" => Needs::Tool,
            "nose" => Needs::Empty,
            _ => Needs::Probe,
        }
    }

    fn local(&self, kind: &str, i: usize) -> f64 {
        self.locv.get(&i).copied().unwrap_or_else(|| locals(kind).get(i).map_or(0.0, |l| l.default))
    }

    /// The macro call for the selected target, or why it can't run.
    fn call(&mut self, t: &Target) -> Result<(String, Option<&'static str>), String> {
        let side = |id: &str| match id {
            "c-fl" | "i-fl" => "front_left",
            "c-fr" | "i-fr" => "front_right",
            "c-bl" | "i-bl" => "back_left",
            _ => "back_right",
        };
        Ok(match t.kind {
            "corner" => (format!("o<probe_{}_top_corner> call", side(t.id)), None),
            "icorner" => (format!("o<probe_{}_inside_corner> call", side(t.id)), None),
            "edge" if self.angle => {
                let e = match t.id { "e-f" => "front", "e-b" => "back", "e-l" => "left", _ => "right" };
                (format!("o<probe_top_{e}_edge_angle> call"), None)
            }
            "edge" => {
                let m = match t.id { "e-f" => "y_plus", "e-b" => "y_minus", "e-l" => "x_plus", _ => "x_minus" };
                (format!("o<probe_{m}_wco> call"), None)
            }
            "boss" => ("o<probe_round_boss> call".into(), None),
            "pocket" => ("o<probe_rect_pocket_center_start> call".into(), None),
            "bore" => ("o<probe_round_pocket_center_start> call".into(), None),
            "slot" => {
                let a = if self.axes == "Y" { "y" } else { "x" };
                if self.ridge { (format!("o<probe_ridge_{a}> call"), None) } else { (format!("o<probe_valley_{a}_center_start> call"), None) }
            }
            "stock" => match &self.flow {
                Some(Flow::StockMeasured { token, .. }) => (format!("o<probe_auto_stock_apply> call [{token}]"), Some("stock-apply")),
                _ => {
                    self.token += 1;
                    let s = match self.start { 'L' => 0, 'R' => 1, 'F' => 2, _ => 3 };
                    (format!("o<probe_auto_stock_center> call [{s}] [{:.3}] [{}]", self.local("stock", 0), self.token), Some("stock-measure"))
                }
            },
            "two" => {
                let axis = if side_axis(self.start) == "X" { 0 } else { 1 };
                let first_dir = if self.start == 'L' || self.start == 'F' { 1 } else { -1 };
                match &self.flow {
                    Some(Flow::TwoFirst { token, axis, dir }) => (format!("o<probe_two_face> call [{axis}] [{}] [2] [{token}]", -dir), Some("two-second")),
                    Some(Flow::TwoReady { token, .. }) => (format!("o<probe_two_face_apply> call [{token}]"), Some("two-apply")),
                    _ => {
                        self.token += 1;
                        (format!("o<probe_two_face> call [{axis}] [{first_dir}] [1] [{}]", self.token), Some("two-first"))
                    }
                }
            }
            "cyl" => (format!("o<probe_a_setup_cylinder_center> call [{:.3}] [1] [0]", self.local("cyl", 0)), None),
            "flat" => ("o<probe_a_rotary> call [1]".into(), None),
            "dia" => (format!("o<probe_a_measure_diameter> call [{:.3}] [1]", self.local("dia", 0)), None),
            "runout" => (format!("o<probe_a_measure_runout> call [{:.3}] [1]", self.local("runout", 0)), None),
            "defl" => (format!("o<probe_a_measure_deflection> call [{:.3}] [{:.3}] [1]", self.local("defl", 0), self.local("defl", 1)), None),
            "ring" => ("o<probe_cal_round_pocket> call".into(), None),
            "setter" => ("o<tool_touch_off> call".into(), None),
            "nose" => ("o<probe_spindle_nose> call".into(), None),
            k => return Err(format!("No routine for {k}")),
        })
    }

    /// The checks the GUI can make before offering a run. The macros make
    /// their own checks too; these only avoid a pointless start.
    fn check(&self, t: &Target) -> Result<(), String> {
        let s = self.status.as_ref().ok_or("Not connected to LinuxCNC")?;
        if !s.power_on() {
            return Err("Turn the machine on first".into());
        }
        if !s.all_homed() {
            return Err("Home the machine first".into());
        }
        if s.busy() || self.run.is_some() {
            return Err("Wait for the running command to finish".into());
        }
        let tool = s.tool_in_spindle;
        match Self::needs(t.kind) {
            Needs::Probe if tool != self.probe_tool => return Err(format!("Load the probe (T{}) first", self.probe_tool)),
            Needs::Tool if tool == self.probe_tool || tool <= 0 => return Err("Load the tool to measure (not the probe)".into()),
            Needs::Empty if tool != 0 => return Err("Unload the spindle (T0) before probing the nose".into()),
            _ => {}
        }
        if t.kind == "nose" {
            if let Some(io) = &self.io {
                if io.read().nose_reference == Some(true) {
                    return Err("The spindle nose reference is already valid".into());
                }
            }
        }
        if Self::needs(t.kind) == Needs::Probe {
            if !s.active_gcodes.contains(&400) {
                return Err("Cancel cutter compensation (G40) before probing".into());
            }
            if s.g92_offset.iter().any(|v| *v != 0.0) {
                return Err("Clear G52/G92 offsets before probing".into());
            }
            let w = self.target_wcs();
            if (self.rotation)(w) != 0.0 {
                return Err(format!("Probing needs an unrotated work system; {} is rotated", WCS[w]));
            }
            if self.pv(3031) != 0.0 {
                return Err("WCO rotation (#3031) is not supported here; set it OFF".into());
            }
        }
        if ["cyl", "flat", "dia", "runout", "defl"].contains(&t.kind) && !s.has_axis(3) {
            return Err("The A axis is not configured".into());
        }
        Ok(())
    }

    fn active(&self) -> usize {
        self.status.as_ref().map_or(0, |s| (s.g5x_index - 1).clamp(0, 8) as usize)
    }

    fn target_wcs(&self) -> usize {
        if self.mode == "find" { self.wcs.unwrap_or_else(|| self.active()) } else { self.active() }
    }

    /// `#n=v` assignments, a few per line (LinuxCNC lines are short).
    fn assign_lines(vals: impl IntoIterator<Item = (u16, f64)>) -> Vec<String> {
        let words: Vec<String> = vals.into_iter().map(|(n, v)| format!("#{n}={v:.6}")).collect();
        words.chunks(6).map(|c| c.join(" ")).collect()
    }

    fn start(&mut self) -> Result<(), String> {
        let t = self.selected();
        self.check(&t)?;
        let (call, flow_step) = self.call(&t)?;
        let mut steps = Vec::new();
        let mut restore = Vec::new();
        let active = self.active();
        let target = self.target_wcs();
        if target != active {
            steps.push(WCS[target].to_string());
        }
        // Run values that differ from the defaults; FIND writes the WCO,
        // MEASURE and CAL report only.
        let mut run: BTreeMap<u16, f64> = self.runv.clone();
        if self.mode == "find" && Self::needs(t.kind) == Needs::Probe {
            run.insert(3030, 0.0);
        }
        run.retain(|n, v| self.defaults.get(n) != Some(v));
        if !run.is_empty() {
            steps.extend(Self::assign_lines(run.iter().map(|(n, v)| (*n, *v))));
            restore.extend(Self::assign_lines(run.keys().map(|n| (*n, self.defaults.get(n).copied().unwrap_or(0.0)))));
            self.mark_dirty(true);
        }
        steps.push(call);
        if target != active {
            restore.push(WCS[active].to_string());
        }
        self.results.clear();
        let what = self.selected().label;
        self.result_text = format!("Running: {what}");
        self.run = Some(Run { steps, restore, sent: None, failed: None, restoring: false, what, kind: t.kind, flow_step });
        self.pump();
        Ok(())
    }

    fn stop(&mut self) {
        self.machine.send(Command::Abort);
        if let Some(r) = &mut self.run {
            if !r.restoring {
                r.failed.get_or_insert_with(|| "Stopped".into());
                r.steps.clear();
                r.sent = None;
            }
        }
    }

    /// Advance the run on each status: one MDI line at a time, each sent
    /// once the previous one finished.
    fn pump(&mut self) {
        let Some(s) = self.status.clone() else { return };
        let Some(run) = &mut self.run else { return };
        if let Some((at, serial, seen_busy)) = &mut run.sent {
            if s.busy() {
                *seen_busy = true;
                return;
            }
            let echoed = s.echo_serial > *serial;
            if !(echoed && (*seen_busy || at.elapsed() > Duration::from_millis(300))) {
                if at.elapsed() < Duration::from_secs(5) {
                    return;
                }
                if !echoed {
                    run.failed.get_or_insert_with(|| "LinuxCNC did not take the command".into());
                }
            }
            run.sent = None;
        }
        if run.failed.is_some() && !run.restoring {
            run.steps.clear();
        }
        if run.steps.is_empty() && !run.restoring {
            run.restoring = true;
            run.steps = std::mem::take(&mut run.restore);
        }
        if run.steps.is_empty() {
            let run = self.run.take().unwrap();
            self.finish(run);
            return;
        }
        let line = run.steps.remove(0);
        run.sent = Some((Instant::now(), s.echo_serial, false));
        self.machine.send(Command::Mdi(line));
    }

    fn finish(&mut self, run: Run) {
        self.mark_dirty(false);
        let results = std::mem::take(&mut self.results);
        if let Some(e) = &run.failed {
            self.result_text = format!("{} failed: {e}", run.what);
            self.flow = None;
            self.note(format!("Probe routine failed: {e}"), true);
            return;
        }
        let num = |name: &str| results.iter().find(|(n, _)| n == name).and_then(|(_, v)| v.first().copied());
        let mut lines: Vec<String> = Vec::new();
        match run.flow_step {
            Some("stock-measure") => {
                if let Some((_, v)) = results.iter().find(|(n, _)| n == "auto_stock_panel.receive_result") {
                    if v.len() == 9 && v[0] as u32 == self.token {
                        self.flow = Some(Flow::StockMeasured { token: self.token, center: (v[7], v[8]), size: (v[5], v[6]) });
                        lines.push(format!("Stock {:.3} × {:.3}", v[5], v[6]));
                        lines.push("Hold APPLY to set X0 Y0 at the center".into());
                    }
                }
                if self.flow.is_none() {
                    lines.push("No result came back; nothing written".into());
                }
            }
            Some("stock-apply") => {
                self.flow = None;
                lines.push(format!("Center written to {}", WCS[self.target_wcs()]));
            }
            Some("two-first") => {
                if let Some((_, v)) = results.iter().find(|(n, _)| n == "two_face_probe_panel.receive_result") {
                    if v.len() == 6 && v[1] as u32 == self.token {
                        let axis = if side_axis(self.start) == "X" { 0 } else { 1 };
                        let dir = if self.start == 'L' || self.start == 'F' { 1 } else { -1 };
                        self.flow = Some(Flow::TwoFirst { token: self.token, axis, dir });
                        lines.push(format!("First face at {:.3}", v[2]));
                        lines.push("Jog to the opposite side, then hold to probe it".into());
                    }
                }
                if self.flow.is_none() {
                    lines.push("No result came back".into());
                }
            }
            Some("two-second") => {
                let tok = match &self.flow { Some(Flow::TwoFirst { token, .. }) => *token, _ => 0 };
                self.flow = None;
                if let Some((_, v)) = results.iter().find(|(n, _)| n == "two_face_probe_panel.receive_result") {
                    if v.len() == 6 && v[1] as u32 == tok {
                        self.flow = Some(Flow::TwoReady { token: tok, width: v[4] });
                        lines.push(format!("Width {:.3}, center {:.3}", v[4], v[5]));
                        lines.push("Hold APPLY to set zero at the center".into());
                    }
                }
                if self.flow.is_none() {
                    lines.push("No result came back".into());
                }
            }
            Some("two-apply") => {
                self.flow = None;
                lines.push(format!("Center written to {}", WCS[self.target_wcs()]));
            }
            _ => {
                for (name, v) in &results {
                    if let Some(x) = v.first() {
                        lines.push(format!("{} {}", result_label(name), fmt3(*x)));
                    }
                }
                if self.mode == "find" && Self::needs(run.kind) == Needs::Probe {
                    lines.push(format!("Written to {}", WCS[self.target_wcs()]));
                }
                if lines.is_empty() {
                    lines.push("Done".into());
                }
                let _ = num;
            }
        }
        self.result_text = lines.join("\n");
        self.note(format!("{}: done", run.what), false);
    }

    /// A macro's `(DEBUG, EVAL[...])` line. Returns true when it was one.
    fn message(&mut self, m: &Message) -> bool {
        let Some((name, args)) = parse_eval(&m.text) else {
            if m.kind == MessageKind::Error {
                if let Some(r) = &mut self.run {
                    r.failed.get_or_insert_with(|| m.text.trim().to_string());
                }
            }
            return false;
        };
        // Calibration results become the new defaults.
        if let Some(n) = name.rsplit('_').next().and_then(|n| n.split('.').next()).and_then(|n| n.parse::<u16>().ok()) {
            if (3004..=3037).contains(&n) {
                if let Some(v) = args.first() {
                    self.defaults.insert(n, *v);
                    self.runv.remove(&n);
                    self.save_defaults();
                }
            }
        }
        self.results.push((name, args));
        true
    }

    fn mark_dirty(&mut self, on: bool) {
        self.restore_pending = on;
        self.state.set("probe.restore-pending", if on { 1 } else { 0 });
    }

    fn save_defaults(&mut self) {
        let list: Vec<String> = self.defaults.iter().map(|(n, v)| format!("{n}={v}")).collect();
        self.state.set_list("probe.default.", list);
    }

    fn note(&self, text: impl Into<String>, error: bool) {
        if let Some(ui) = self.ui.upgrade() {
            let live = ui.global::<Live>();
            live.set_message(text.into().into());
            live.set_message_error(error);
        }
    }

    fn show(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        live.set_pr_mode(self.mode.into());
        let shapes = SHAPES.iter().find(|(m, _)| *m == self.mode).map_or(&[][..], |(_, s)| s);
        let items: Vec<ProbeItem> = shapes.iter().map(|(id, l)| ProbeItem { id: (*id).into(), label: (*l).into(), on: *id == self.shape }).collect();
        live.set_pr_shapes(ModelRc::new(VecModel::from(items)));
        live.set_pr_shape_label(
            match self.mode {
                "find" => "WHAT ARE YOU PROBING?",
                "meas" => "WHAT TO MEASURE?",
                _ => "WHAT TO CALIBRATE",
            }
            .into(),
        );
        let targets = self.targets();
        let sel = self.selected();
        live.set_pr_title(sel.label.clone().into());
        live.set_pr_hint(if targets.len() > 1 { "TAP A TARGET" } else { "" }.into());
        // Picture
        let (outline, arrows, heads) = picture(self, &sel);
        live.set_pr_outline(outline.into());
        live.set_pr_arrows(arrows.into());
        live.set_pr_heads(heads.into());
        let mut spots: Vec<ProbeSpot> = Vec::new();
        if targets.len() > 1 {
            for t in &targets {
                spots.push(ProbeSpot {
                    id: t.id.into(),
                    x: t.x as f32,
                    y: t.y as f32,
                    w: t.w as f32,
                    h: t.h as f32,
                    r: if t.id == "ctr" { 26.0 } else { 22.0 },
                    on: t.id == sel.id,
                    label: "".into(),
                });
            }
        }
        if self.shape == "block" && sel.id == "ctr" {
            for (k, x, y) in [('F', 200.0, 262.0), ('B', 200.0, 38.0), ('L', 46.0, 150.0), ('R', 354.0, 150.0)] {
                let on = k == self.start;
                spots.push(ProbeSpot { id: format!("start-{k}").into(), x, y, w: 96.0, h: 34.0, r: 17.0, on, label: if on { "START HERE" } else { "START" }.into() });
            }
        }
        live.set_pr_spots(ModelRc::new(VecModel::from(spots)));
        // Options under the picture
        let mut opts: Vec<ProbeItem> = Vec::new();
        let it = |id: &str, label: String, on: bool| ProbeItem { id: id.into(), label: label.into(), on };
        if self.mode == "find" && sel.kind == "edge" {
            opts.push(it("angle", "ALSO MEASURE ANGLE".into(), self.angle));
        }
        if self.shape == "block" && sel.id == "ctr" {
            opts.push(it("both", "BOTH AXES".into(), self.both));
            opts.push(it("one", format!("{} ONLY", side_axis(self.start)), !self.both));
        }
        if self.shape == "slot" {
            opts.push(it("ridge", "RIDGE".into(), self.ridge));
            opts.push(it("valley", "SLOT".into(), !self.ridge));
            opts.push(it("X", "ACROSS X".into(), self.axes != "Y"));
            opts.push(it("Y", "ACROSS Y".into(), self.axes == "Y"));
        }
        live.set_pr_opts(ModelRc::new(VecModel::from(opts)));
        // Values
        let key = self.key_params(sel.kind);
        let row = |n: u16| {
            let p = param(n);
            ProbeParam {
                id: n.to_string().into(),
                label: p.map_or("", |p| p.label).into(),
                value: fmt_value(n, self.pv(n)).into(),
                unit: p.map_or("", |p| p.unit).into(),
                changed: self.runv.contains_key(&n),
                def: fmt_value(n, self.defaults.get(&n).copied().unwrap_or(0.0)).into(),
            }
        };
        let mut fields: Vec<ProbeParam> = key.iter().map(|n| row(*n)).collect();
        for (i, l) in locals(sel.kind).iter().enumerate() {
            fields.push(ProbeParam {
                id: format!("L{i}").into(),
                label: l.label.into(),
                value: fmt_value(0, self.local(sel.kind, i)).into(),
                unit: "mm".into(),
                changed: self.locv.contains_key(&i),
                def: fmt_value(0, l.default).into(),
            });
        }
        live.set_pr_params(ModelRc::new(VecModel::from(fields)));
        let more: Vec<ProbeParam> = self.used_params(sel.kind).into_iter().filter(|n| !key.contains(n)).map(row).collect();
        live.set_pr_more(ModelRc::new(VecModel::from(more)));
        live.set_pr_changed(!self.runv.is_empty() || !self.locv.is_empty());
        live.set_pr_defaults(ModelRc::new(VecModel::from(PARAMS.iter().map(|p| {
            let mut r = row(p.n);
            r.value = fmt_value(p.n, self.defaults.get(&p.n).copied().unwrap_or(0.0)).into();
            r.changed = false;
            r.def = p.group.into();
            r
        }).collect::<Vec<_>>())));
        // Routine note and buttons
        let what = match sel.kind {
            "corner" => "outside top corner (touches top Z first)".to_string(),
            "icorner" => "inside corner".into(),
            "edge" => format!("single edge{}", if self.angle { " + edge angle (touches top Z)" } else { "" }),
            "stock" => "Auto Stock: walks the outside, then APPLY".into(),
            "two" => "Two Faces: first face, opposite face, APPLY".into(),
            "boss" => "round boss (touches top Z first)".into(),
            "pocket" => "rectangular pocket from its center".into(),
            "bore" => "round pocket from its center".into(),
            "slot" => if self.ridge { "ridge (touches top Z)" } else { "slot from its center" }.into(),
            "cyl" => "rotary setup (Y, Z)".into(),
            "flat" => "rotary A0 on a flat".into(),
            "dia" => "measure diameter".into(),
            "runout" => "measure runout".into(),
            "defl" => "measure deflection".into(),
            "ring" => "probe tip calibration in a ring".into(),
            "setter" => "tool length on the setter".into(),
            _ => "spindle nose reference".into(),
        };
        let call = {
            // Show the macro name without consuming a token.
            let mut tmp = Probe { token: self.token, flow: self.flow.clone(), ..self.shallow() };
            tmp.call(&sel).map(|(c, _)| c).unwrap_or_default()
        };
        live.set_pr_routine(format!("Runs: {what}\n{}", call.split_whitespace().next().unwrap_or("")).into());
        live.set_pr_note(
            match self.mode {
                "find" => "",
                "meas" => "Reports only. Work offsets are not changed.",
                _ => "Updates probe or tool data.",
            }
            .into(),
        );
        live.set_pr_show_wcs(self.mode == "find");
        live.set_pr_wcs(WCS[self.target_wcs()].into());
        live.set_pr_result(if self.result_text.is_empty() { "No measurement yet.".into() } else { self.result_text.clone().into() });
        let needs = Self::needs(sel.kind);
        let tool = self.status.as_ref().map_or(-1, |s| s.tool_in_spindle);
        let ok = match needs {
            Needs::Probe => tool == self.probe_tool,
            Needs::Tool => tool > 0 && tool != self.probe_tool,
            Needs::Empty => tool == 0,
        };
        let (pill, kind) = match needs {
            Needs::Probe if ok => (format!("T{} LOADED · WIRED", self.probe_tool), "good"),
            Needs::Probe => (format!("T{} NOT LOADED", self.probe_tool), "warn"),
            Needs::Tool if ok => (format!("T{tool} IN SPINDLE"), "good"),
            Needs::Tool => ("LOAD THE TOOL TO MEASURE".into(), "warn"),
            Needs::Empty if ok => ("SPINDLE EMPTY".into(), "good"),
            Needs::Empty => ("UNLOAD THE SPINDLE".into(), "warn"),
        };
        live.set_pr_pill(pill.into());
        live.set_pr_pill_kind(kind.into());
        live.set_pr_load_label(match needs { Needs::Probe => format!("LOAD PROBE T{}", self.probe_tool), _ => "UNLOAD SPINDLE".into() }.into());
        live.set_pr_load_enabled(!ok && needs != Needs::Tool);
        let run_label = match (&self.flow, sel.kind) {
            (Some(Flow::StockMeasured { .. }), "stock") | (Some(Flow::TwoReady { .. }), "two") => "HOLD TO APPLY".to_string(),
            (Some(Flow::TwoFirst { .. }), "two") => "HOLD: OPPOSITE FACE".into(),
            _ if self.mode == "find" => "HOLD TO PROBE".into(),
            _ if self.mode == "meas" => "HOLD TO MEASURE".into(),
            _ if needs != Needs::Probe => "HOLD TO MEASURE TOOL".into(),
            _ => "HOLD TO CALIBRATE".into(),
        };
        live.set_pr_run_label(run_label.into());
        live.set_pr_running(self.run.is_some());
        live.set_pr_ready(self.check(&sel).err().unwrap_or_default().into());
    }

    /// A copy of the selection, for computing labels without side effects.
    fn shallow(&self) -> Probe {
        Probe {
            mode: self.mode,
            shape: self.shape,
            tgt: self.tgt,
            axes: self.axes,
            angle: self.angle,
            start: self.start,
            both: self.both,
            ridge: self.ridge,
            wcs: self.wcs,
            runv: BTreeMap::new(),
            locv: self.locv.clone(),
            defaults: BTreeMap::new(),
            results: Vec::new(),
            result_text: String::new(),
            flow: None,
            token: 0,
            run: None,
            status: None,
            state: Settings::memory(),
            restore_pending: false,
            probe_tool: self.probe_tool,
            machine: self.machine.clone(),
            io: None,
            ui: self.ui.clone(),
            rotation: Box::new(|_| 0.0),
        }
    }

    fn refresh(&self) {
        if let Some(ui) = self.ui.upgrade() {
            self.show(&ui);
        }
    }

    /// Keep the target and options consistent after a change.
    fn settle(&mut self) {
        let shapes = SHAPES.iter().find(|(m, _)| *m == self.mode).map_or(&[][..], |(_, s)| s);
        if !shapes.iter().any(|(id, _)| *id == self.shape) {
            self.shape = shapes[0].0;
        }
        if self.shape == "block" {
            self.axes = if self.both { "XY" } else { side_axis(self.start) };
        } else if self.shape == "slot" && self.axes == "XY" {
            self.axes = "X";
        }
        let t = self.targets();
        if !t.iter().any(|t| t.id == self.tgt) {
            self.tgt = t.iter().find(|t| t.id == "c-fl").unwrap_or(&t[0]).id;
        }
    }

    fn set_value(&mut self, id: &str, v: f64) {
        let v = (v * 1000.0).round() / 1000.0;
        if let Some(i) = id.strip_prefix('L').and_then(|i| i.parse::<usize>().ok()) {
            self.locv.insert(i, v.max(0.0));
            return;
        }
        let Ok(n) = id.parse::<u16>() else { return };
        let mut v = v;
        if let Some(p) = param(n).filter(|p| !p.seg.is_empty()) {
            v = v.clamp(0.0, (p.seg.len() - 1) as f64);
        }
        if self.defaults.get(&n) == Some(&v) {
            self.runv.remove(&n);
        } else {
            self.runv.insert(n, v);
        }
    }

    fn get_value(&self, id: &str) -> f64 {
        if let Some(i) = id.strip_prefix('L').and_then(|i| i.parse::<usize>().ok()) {
            return self.local(self.selected().kind, i);
        }
        id.parse::<u16>().map_or(0.0, |n| self.pv(n))
    }

    /// DEFAULTS: write a parameter and keep it as the default.
    fn set_default(&mut self, n: u16, v: f64) -> Result<(), String> {
        let s = self.status.as_ref().ok_or("Not connected to LinuxCNC")?;
        if !s.power_on() || s.busy() || self.run.is_some() {
            return Err("Defaults change through MDI: machine on and nothing running".into());
        }
        self.machine.send(Command::Mdi(format!("#{n}={v:.6}")));
        self.defaults.insert(n, v);
        self.runv.remove(&n);
        self.save_defaults();
        Ok(())
    }
}

fn fmt3(v: f64) -> String {
    format!("{v:.3}")
}

fn result_label(name: &str) -> String {
    let base = name.split('.').next().unwrap_or(name);
    match base {
        "x_center_probed" => "X center",
        "y_center_probed" => "Y center",
        "x_probed_width" => "X width",
        "y_probed_width" => "Y width",
        "averaged_diam" => "Diameter",
        "edge_angle" => "Edge angle (not applied)",
        "edge_delta" => "Edge delta",
        "z_minus_probed_position" => "Z",
        "measure_diameter_value" => "Diameter",
        "measure_diameter_spread_value" => "Spread",
        "measure_runout_tir_value" => "TIR",
        "measure_runout_high_angle_value" => "High at A",
        "measure_runout_diameter_value" => "Diameter",
        "measure_deflect_y_delta_value" => "Y drift",
        "measure_deflect_z_delta_value" => "Z drift",
        "measure_deflect_y_angle_value" => "Y angle",
        "measure_deflect_z_angle_value" => "Z angle",
        "calibration_offset_3032" => "Calibration offset (saved)",
        "spindle_zero_height_3010" => "Spindle zero height (saved)",
        other => other,
    }
    .to_string()
}

/// `EVAL[vcp.getWidget{"name"}.setValue{1.5}]` → ("name", [1.5]);
/// `...{"panel"}.receive_result{1,2,3}` → ("panel.receive_result", [1,2,3]).
pub fn parse_eval(text: &str) -> Option<(String, Vec<f64>)> {
    let rest = text.trim().strip_prefix("EVAL[vcp.getWidget{\"")?;
    let (name, rest) = rest.split_once("\"}.")?;
    let (method, rest) = rest.split_once('{')?;
    let args = rest.split('}').next()?;
    let vals: Vec<f64> = args.split(',').filter_map(|a| a.trim().parse().ok()).collect();
    let key = if method == "setValue" { name.to_string() } else { format!("{name}.{method}") };
    Some((key, vals))
}

/// SVG path data in a 400×300 box: outline, approach lines, arrowheads.
fn picture(p: &Probe, sel: &Target) -> (String, String, String) {
    let circle = |cx: f64, cy: f64, r: f64| format!("M {} {} A {r} {r} 0 1 0 {} {cy} A {r} {r} 0 1 0 {} {cy} Z ", cx - r, cy, cx + r, cx - r);
    let rect = |x: f64, y: f64, w: f64, h: f64| format!("M {x} {y} L {} {y} L {} {} L {x} {} Z ", x + w, x + w, y + h, y + h);
    let outline = match p.shape {
        "block" => rect(100.0, 70.0, 200.0, 160.0),
        "pocket" => rect(60.0, 40.0, 280.0, 220.0) + &rect(110.0, 80.0, 180.0, 140.0),
        "round" => circle(200.0, 150.0, 80.0),
        "bore" => rect(60.0, 40.0, 280.0, 220.0) + &circle(200.0, 150.0, 60.0),
        "slot" if p.axes == "Y" => rect(60.0, 110.0, 280.0, 80.0),
        "slot" => rect(160.0, 40.0, 80.0, 220.0),
        "cyl" | "dia" | "runout" => circle(200.0, 165.0, 70.0) + "M 200 82 L 200 248 M 110 165 L 290 165",
        "defl" => rect(140.0, 40.0, 40.0, 200.0),
        "ring" => circle(200.0, 150.0, 100.0) + &circle(200.0, 150.0, 66.0),
        "setter" => rect(160.0, 200.0, 80.0, 40.0),
        _ => rect(160.0, 200.0, 80.0, 40.0) + &rect(170.0, 60.0, 60.0, 70.0),
    };
    let mut lines = String::new();
    let mut heads = String::new();
    for [x1, y1, x2, y2] in segs(p, sel) {
        lines += &format!("M {x1:.1} {y1:.1} L {x2:.1} {y2:.1} ");
        let (dx, dy) = (x2 - x1, y2 - y1);
        let l = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (ux, uy) = (dx / l, dy / l);
        let (bx, by) = (x2 - ux * 12.0, y2 - uy * 12.0);
        heads += &format!("M {x2:.1} {y2:.1} L {:.1} {:.1} L {:.1} {:.1} Z ", bx - uy * 6.0, by + ux * 6.0, bx + uy * 6.0, by - ux * 6.0);
    }
    (outline, lines, heads)
}

/// Probe approach moves in picture units (from the mockup).
fn segs(p: &Probe, t: &Target) -> Vec<[f64; 4]> {
    let k = t.kind;
    if k == "corner" || k == "icorner" {
        let dx = if t.x < 200.0 { -1.0 } else { 1.0 };
        let dy = if t.y < 150.0 { -1.0 } else { 1.0 };
        let (x, y) = (t.x, t.y);
        return if k == "icorner" {
            vec![[x - dx * 50.0, y - dy * 30.0, x - dx * 2.0, y - dy * 30.0], [x - dx * 30.0, y - dy * 50.0, x - dx * 30.0, y - dy * 2.0]]
        } else {
            vec![[x + dx * 60.0, y - dy * 30.0, x, y - dy * 30.0], [x - dx * 30.0, y + dy * 60.0, x - dx * 30.0, y]]
        };
    }
    if k == "edge" {
        return if t.w > t.h {
            let y0 = if t.y < 150.0 { 18.0 } else { 282.0 };
            vec![[200.0, y0, 200.0, t.y + 12.0]]
        } else {
            let x0 = if t.x < 200.0 { 40.0 } else { 360.0 };
            let mut v = vec![[x0, 150.0, t.x + 12.0, 150.0]];
            if p.angle {
                v.push([x0, 200.0, t.x + 12.0, 200.0]);
            }
            v
        };
    }
    let stock = |c: char| match c {
        'L' => [40.0, 150.0, 100.0, 150.0],
        'R' => [360.0, 150.0, 300.0, 150.0],
        'B' => [200.0, 18.0, 200.0, 70.0],
        _ => [200.0, 282.0, 200.0, 230.0],
    };
    match k {
        "stock" => {
            let ord = match p.start { 'F' => "FRBL", 'R' => "RBLF", 'B' => "BLFR", _ => "LFRB" };
            ord.chars().map(stock).collect()
        }
        "two" => {
            let opp = match p.start { 'F' => 'B', 'B' => 'F', 'L' => 'R', _ => 'L' };
            vec![stock(p.start), stock(opp)]
        }
        "boss" => vec![[60.0, 150.0, 120.0, 150.0], [340.0, 150.0, 280.0, 150.0], [200.0, 30.0, 200.0, 70.0], [200.0, 270.0, 200.0, 230.0]],
        "pocket" => vec![[200.0, 150.0, 290.0, 150.0], [200.0, 150.0, 110.0, 150.0], [200.0, 150.0, 200.0, 80.0], [200.0, 150.0, 200.0, 220.0]],
        "bore" => vec![[200.0, 150.0, 260.0, 150.0], [200.0, 150.0, 140.0, 150.0], [200.0, 150.0, 200.0, 90.0], [200.0, 150.0, 200.0, 210.0]],
        "slot" if p.ridge => {
            if p.axes == "Y" { vec![[200.0, 60.0, 200.0, 110.0], [200.0, 240.0, 200.0, 190.0]] } else { vec![[110.0, 150.0, 160.0, 150.0], [290.0, 150.0, 240.0, 150.0]] }
        }
        "slot" => {
            if p.axes == "Y" { vec![[200.0, 150.0, 200.0, 112.0], [200.0, 150.0, 200.0, 188.0]] } else { vec![[200.0, 150.0, 162.0, 150.0], [200.0, 150.0, 238.0, 150.0]] }
        }
        "cyl" | "dia" => vec![[200.0, 40.0, 200.0, 92.0], [60.0, 165.0, 127.0, 165.0], [340.0, 165.0, 273.0, 165.0]],
        "runout" => vec![[200.0, 40.0, 200.0, 92.0]],
        "flat" => vec![[170.0, 40.0, 170.0, 92.0], [230.0, 40.0, 230.0, 92.0]],
        "defl" => vec![[260.0, 150.0, 186.0, 150.0]],
        "ring" => vec![[200.0, 150.0, 136.0, 150.0], [200.0, 150.0, 264.0, 150.0], [200.0, 150.0, 200.0, 86.0]],
        "setter" => vec![[200.0, 40.0, 200.0, 196.0]],
        "nose" => vec![[200.0, 136.0, 200.0, 196.0]],
        _ => vec![],
    }
}

/// Keypad values: "prun-<id>" (this run) and "pdef-<n>" (saved default).
struct Keys(Rc<RefCell<Probe>>);

impl KeypadTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        let p = self.0.borrow();
        if let Some(id) = what.strip_prefix("prun-") {
            let name = if let Some(i) = id.strip_prefix('L').and_then(|i| i.parse::<usize>().ok()) {
                locals(p.selected().kind).get(i).map_or("", |l| l.label)
            } else {
                id.parse::<u16>().ok().and_then(param).map_or("", |p| p.label)
            };
            return Some(format!("{} · THIS RUN", name.to_uppercase()));
        }
        let n = what.strip_prefix("pdef-")?.parse::<u16>().ok()?;
        Some(format!("{} · DEFAULT #{n}", param(n)?.label.to_uppercase()))
    }
    fn current(&self, what: &str) -> Option<f64> {
        let p = self.0.borrow();
        if let Some(id) = what.strip_prefix("prun-") {
            return Some(p.get_value(id));
        }
        let n = what.strip_prefix("pdef-")?.parse::<u16>().ok()?;
        Some(p.defaults.get(&n).copied().unwrap_or(0.0))
    }
    fn apply(&self, what: &str, v: f64) -> Option<Result<(), String>> {
        let mut p = self.0.borrow_mut();
        let r = if let Some(id) = what.strip_prefix("prun-") {
            p.set_value(id, v);
            Ok(())
        } else {
            let n = what.strip_prefix("pdef-")?.parse::<u16>().ok()?;
            p.set_default(n, v)
        };
        p.refresh();
        Some(r)
    }
}

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, io: Option<Rc<dyn Io>>, sim: bool, rotation: Box<dyn Fn(usize) -> f64>) -> Rc<RefCell<Probe>> {
    let probe_tool = cfg.get("PROBE", "TOOL_NUMBER").and_then(|v| v.trim().parse().ok()).unwrap_or(99);
    // Defaults: the parameter file, unless a run never restored them.
    let state = if sim { Settings::memory() } else { Settings::at("probe") };
    let restore_pending = state.get("probe.restore-pending") == Some("1");
    let mut defaults: BTreeMap<u16, f64> = BTreeMap::new();
    if restore_pending {
        for v in state.list("probe.default.") {
            if let Some((n, v)) = v.split_once('=') {
                if let (Ok(n), Ok(v)) = (n.parse(), v.parse()) {
                    defaults.insert(n, v);
                }
            }
        }
    }
    if defaults.is_empty() {
        let path = cfg.dir().join(cfg.get("RS274NGC", "PARAMETER_FILE").unwrap_or("linuxcnc.var"));
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let mut w = line.split_whitespace();
                if let (Some(Ok(n)), Some(Ok(v))) = (w.next().map(str::parse::<u16>), w.next().map(str::parse::<f64>)) {
                    if (3004..=3037).contains(&n) {
                        defaults.insert(n, v);
                    }
                }
            }
        }
    }
    let probe = Rc::new(RefCell::new(Probe {
        mode: "find",
        shape: "block",
        tgt: "c-fl",
        axes: "XY",
        angle: false,
        start: 'F',
        both: true,
        ridge: true,
        wcs: None,
        runv: BTreeMap::new(),
        locv: BTreeMap::new(),
        defaults,
        results: Vec::new(),
        result_text: String::new(),
        flow: None,
        token: (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_secs()) % 100_000) as u32,
        run: None,
        status: None,
        state,
        restore_pending,
        probe_tool,
        machine: machine.clone(),
        io,
        ui: ui.as_weak(),
        rotation,
    }));
    probe.borrow_mut().save_defaults();
    crate::manual::add_keypad_target(Rc::new(Keys(probe.clone())));
    let actions = ui.global::<Actions>();
    let on = |f: Box<dyn Fn(&mut Probe)>| {
        let p = probe.clone();
        move || {
            let mut p = p.borrow_mut();
            f(&mut p);
            p.refresh();
        }
    };
    let p = probe.clone();
    actions.on_probe_mode(move |m| {
        let mut p = p.borrow_mut();
        p.mode = match m.as_str() { "meas" => "meas", "cal" => "cal", _ => "find" };
        p.shape = "";
        p.flow = None;
        p.locv.clear();
        p.settle();
        p.refresh();
    });
    let p = probe.clone();
    actions.on_probe_shape(move |s| {
        let mut p = p.borrow_mut();
        let all = SHAPES.iter().flat_map(|(_, l)| l.iter());
        if let Some((id, _)) = all.into_iter().find(|(id, _)| *id == s.as_str()) {
            p.shape = id;
        }
        p.flow = None;
        p.locv.clear();
        p.settle();
        p.refresh();
    });
    let p = probe.clone();
    actions.on_probe_target(move |id| {
        let mut p = p.borrow_mut();
        if let Some(k) = id.strip_prefix("start-").and_then(|k| k.chars().next()) {
            p.start = k;
        } else if let Some(t) = p.targets().into_iter().find(|t| t.id == id.as_str()) {
            p.tgt = t.id;
        }
        p.flow = None;
        p.settle();
        p.refresh();
    });
    let p = probe.clone();
    actions.on_probe_opt(move |o| {
        let mut p = p.borrow_mut();
        match o.as_str() {
            "angle" => p.angle = !p.angle,
            "both" => p.both = true,
            "one" => p.both = false,
            "ridge" => p.ridge = true,
            "valley" => p.ridge = false,
            "X" => p.axes = "X",
            "Y" => p.axes = "Y",
            _ => {}
        }
        p.flow = None;
        p.settle();
        p.refresh();
    });
    let p = probe.clone();
    actions.on_probe_step(move |id, dir| {
        let mut p = p.borrow_mut();
        let step = id.parse::<u16>().map_or(0.5, step_of);
        let v = (p.get_value(&id) + step * dir as f64).max(0.0);
        p.set_value(&id, v);
        p.refresh();
    });
    let p = probe.clone();
    actions.on_probe_reset(move |id| {
        let mut p = p.borrow_mut();
        if let Some(i) = id.strip_prefix('L').and_then(|i| i.parse::<usize>().ok()) {
            p.locv.remove(&i);
        } else if let Ok(n) = id.parse::<u16>() {
            p.runv.remove(&n);
        }
        p.refresh();
    });
    actions.on_probe_reset_all(on(Box::new(|p| {
        p.runv.clear();
        p.locv.clear();
    })));
    let p = probe.clone();
    actions.on_probe_wcs(move |code| {
        let mut p = p.borrow_mut();
        p.wcs = WCS.iter().position(|c| *c == code.as_str());
        p.refresh();
    });
    let p = probe.clone();
    actions.on_probe_load(move || {
        let p = p.borrow();
        let line = match Probe::needs(p.selected().kind) {
            Needs::Probe => format!("M6 T{} G43", p.probe_tool),
            _ => "M6 T0".into(),
        };
        p.machine.send(Command::Mdi(line));
    });
    let p = probe.clone();
    actions.on_probe_run(move || {
        let mut p = p.borrow_mut();
        if let Err(e) = p.start() {
            p.note(e, true);
        }
        p.refresh();
    });
    actions.on_probe_stop(on(Box::new(|p| p.stop())));
    probe.borrow().refresh();
    probe
}

pub fn apply_status(ui: &MainWindow, probe: &Rc<RefCell<Probe>>, s: &Status) {
    let mut p = probe.borrow_mut();
    let was = p.status.as_ref().map(|o| (o.tool_in_spindle, o.power_on(), o.busy(), o.g5x_index));
    p.status = Some(s.clone());
    // A run that never restored its defaults (crash): restore them now.
    if p.restore_pending && p.run.is_none() && s.power_on() && !s.busy() {
        let lines = Probe::assign_lines(p.defaults.iter().map(|(n, v)| (*n, *v)));
        p.run = Some(Run { steps: Vec::new(), restore: lines, sent: None, failed: None, restoring: false, what: "Restore probe defaults".into(), kind: "", flow_step: None });
        p.restore_pending = false;
    }
    p.pump();
    if was != Some((s.tool_in_spindle, s.power_on(), s.busy(), s.g5x_index)) || p.run.is_some() {
        p.show(ui);
    }
}

/// Steps finish without a status change (an instant MDI), so poll too.
pub fn start_timer(probe: &Rc<RefCell<Probe>>) -> slint::Timer {
    let timer = slint::Timer::default();
    let p = probe.clone();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        let mut p = p.borrow_mut();
        if p.run.is_some() {
            p.pump();
            p.refresh();
        }
    });
    timer
}

pub fn message(ui: &MainWindow, probe: &Rc<RefCell<Probe>>, m: &Message) -> bool {
    let mut p = probe.borrow_mut();
    let eaten = p.message(m);
    if eaten {
        p.show(ui);
    }
    eaten
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_lines() {
        assert_eq!(parse_eval("EVAL[vcp.getWidget{\"x_center_probed\"}.setValue{12.5}]"), Some(("x_center_probed".into(), vec![12.5])));
        assert_eq!(
            parse_eval("EVAL[vcp.getWidget{\"two_face_probe_panel\"}.receive_result{1,42,10.000000,30.000000,20.000000,20.000000}]"),
            Some(("two_face_probe_panel.receive_result".into(), vec![1.0, 42.0, 10.0, 30.0, 20.0, 20.0]))
        );
        assert_eq!(parse_eval("Probe routine complete"), None);
    }

    #[test]
    fn assignments_split() {
        let l = Probe::assign_lines((3004..3018).map(|n| (n, 1.0)));
        assert_eq!(l.len(), 3);
        assert!(l.iter().all(|l| l.len() < 200));
        assert_eq!(l[0].split_whitespace().next(), Some("#3004=1.000000"));
    }

    #[test]
    fn values_format() {
        assert_eq!(fmt_value(3019, 2.0), "2");
        assert_eq!(fmt_value(3032, 0.059), "0.059");
        assert_eq!(fmt_value(3030, 1.0), "POSITION ONLY");
    }
}
