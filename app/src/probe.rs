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
//! - Defaults: millgui.toml's `[probe.defaults]` and `[toolsetter]` values
//!   (probe_defaults.rs), else the parameter file's, else starting values;
//!   #3014 is `[probe]tool`. When these differ from what LinuxCNC holds,
//!   the whole set is written once LinuxCNC is quiet (as a restore). If
//!   the marker says an earlier run never restored them, the saved copy
//!   stands in for the parameter file. A default changed on DEFAULTS or in
//!   SETUP is saved to millgui.toml. Calibration macros change #3010/#3032
//!   themselves; their reported value becomes the new default (and is
//!   saved) so the restore keeps it.
//! - Results arrive as the macros' `(DEBUG, EVAL[vcp.getWidget{..}...])`
//!   lines; they are shown, never trusted to have written anything.
//! - FIND writes to the chosen WCS by making it active for the run and
//!   switching back afterwards.

use crate::config::Config;
use crate::manual::KeypadTarget;
use crate::settings::Settings;
use crate::toolchange::Io;
use crate::{Actions, Live, MainWindow, ProbeItem, ProbeOpt, ProbeParam, ProbeSpot};
use machine::{Command, Handle, Message, MessageKind, Status};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Every subroutine a PROBE routine calls (`<name>.ngc` on SUBROUTINE_PATH).
pub const MACROS: &[&str] = &[
    "probe_a_measure_deflection", "probe_a_measure_diameter", "probe_a_measure_runout", "probe_a_rotary",
    "probe_a_setup_cylinder_center", "probe_auto_stock_center", "probe_back_left_inside_corner",
    "probe_back_left_top_corner", "probe_back_right_inside_corner", "probe_back_right_top_corner",
    "probe_cal_round_pocket", "probe_front_left_inside_corner", "probe_front_left_top_corner",
    "probe_front_right_inside_corner", "probe_front_right_top_corner", "probe_rect_pocket_center_start",
    "probe_ridge_x", "probe_ridge_y", "probe_round_boss", "probe_round_pocket_center_start",
    "probe_spindle_nose", "probe_top_back_edge_angle", "probe_top_front_edge_angle",
    "probe_top_left_edge_angle", "probe_top_right_edge_angle", "probe_two_face",
    "probe_valley_x_center_start", "probe_valley_y_center_start", "probe_x_minus_wco", "probe_x_plus_wco",
    "probe_y_minus_wco", "probe_y_plus_wco", "probe_z_minus_wco", "tool_touch_off",
];

/// How long after a failed restore of the probe defaults to try again.
const RESTORE_RETRY: Duration = Duration::from_secs(30);

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
    s(3036, "Cal axis", &["X+Y", "X", "Y"], "cal"),
    p(3037, "Breakage tolerance", "mm", "cal"),
];

fn param(n: u16) -> Option<&'static Param> {
    PARAMS.iter().find(|p| p.n == n)
}

/// The machine's units.
fn units() -> crate::machine_info::Units {
    crate::machine_info::get().units
}

/// A value's unit on this machine: PARAMS names them in mm, which on an
/// inch machine are inches ("in", "in/min"): the macros work in machine
/// units.
fn unit_of(unit: &str, u: crate::machine_info::Units) -> &'static str {
    match unit {
        "mm" => u.name(),
        "mm/min" => u.feed(),
        _ => "",
    }
}

/// The − / + step for a value, in machine units.
fn step_of(n: u16, u: crate::machine_info::Units) -> f64 {
    let p = param(n);
    if p.is_some_and(|p| !p.seg.is_empty()) || n == crate::probe_defaults::PROBE_TOOL {
        1.0
    } else if p.is_some_and(|p| p.unit.contains("/min")) {
        u.pick(10.0, 0.5)
    } else if [3019, 3021, 3009, 3022].contains(&n) {
        u.pick(0.5, 0.02)
    } else if n == 3033 || n == 3032 {
        u.pick(0.1, 0.005)
    } else {
        u.pick(1.0, 0.05)
    }
}

fn fmt_value(n: u16, v: f64) -> String {
    fmt_in(n, v, units())
}

/// A value to the unit's decimals (3 in mm, 4 in inches), trailing zeros
/// dropped.
fn fmt_in(n: u16, v: f64, u: crate::machine_info::Units) -> String {
    if let Some(p) = param(n).filter(|p| !p.seg.is_empty()) {
        return p.seg.get(v as usize).map_or_else(|| format!("{v}"), |s| s.to_string());
    }
    let s = u.fmt(v);
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

/// Picture spot sizes in picture units (the picture is drawn at about
/// 1.2–1.5 px per unit, so each spot is at least 56 px across).
const CORNER_R: f64 = 28.0;
const CENTER_R: f64 = 34.0;
const EDGE_T: f64 = 46.0;

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
    ("find", &[("block", "BLOCK / STOCK"), ("top", "TOP FACE Z"), ("round", "ROUND BOSS"), ("pocket", "POCKET"), ("bore", "BORE"), ("slot", "RIDGE / SLOT"), ("cyl", "ON THE ROTARY")]),
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
/// Lengths in machine units: a starting value for each.
#[derive(Clone, Copy)]
struct Local {
    label: &'static str,
    mm: f64,
    inch: f64,
}
impl Local {
    fn default(&self) -> f64 {
        units().pick(self.mm, self.inch)
    }
}
fn locals(kind: &str) -> &'static [Local] {
    match kind {
        "stock" => &[Local { label: "Max span", mm: 150.0, inch: 6.0 }],
        "cyl" => &[Local { label: "Reference dia est.", mm: 30.0, inch: 1.25 }],
        "dia" | "runout" => &[Local { label: "Diameter est.", mm: 25.0, inch: 1.0 }],
        "defl" => &[Local { label: "Diameter est.", mm: 25.0, inch: 1.0 }, Local { label: "X span", mm: 50.0, inch: 2.0 }],
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
    sent: Option<(Instant, machine::Serial, bool)>,
    failed: Option<String>,
    restoring: bool,
    /// A restore line was refused or failed: the defaults may not be back.
    restore_failed: bool,
    what: String,
    kind: &'static str,
    flow_step: Option<&'static str>,
    /// No line before this: LinuxCNC drops an MDI line sent while it is
    /// still aborting, and the restore after a STOP must not be lost.
    not_before: Option<Instant>,
    /// When the last line finished. The macro's result lines come over
    /// LinuxCNC's error channel and can arrive just after the status shows
    /// it idle, so the run ends a moment later (`RESULT_GRACE`).
    done_at: Option<Instant>,
}

const RESULT_GRACE: Duration = Duration::from_millis(400);
/// How long after a STOP before the defaults are put back.
const ABORT_SETTLE: Duration = Duration::from_millis(600);

/// How long LinuxCNC must sit still before millgui writes the defaults.
const QUIET_FOR: Duration = Duration::from_secs(3);

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
    /// No restore attempt before this (after one failed).
    restore_after: Option<Instant>,
    /// Since when LinuxCNC has been on, homed and still (`maybe_restore`).
    quiet_since: Option<Instant>,
    /// A job with probing ops was started; see `job_started`.
    job_armed: bool,
    probe_tool: i32,
    /// Where each owned default came from (probe_defaults::DEFS).
    sources: BTreeMap<u16, crate::probe_defaults::Source>,
    /// The millgui.toml defaults are saved to, and the file a new one
    /// starts from; None in the simulator.
    toml: Option<(std::path::PathBuf, Option<std::path::PathBuf>)>,
    machine: Handle,
    io: Option<Rc<dyn Io>>,
    ui: slint::Weak<MainWindow>,
    rotation: Box<dyn Fn(usize) -> f64>,
    /// This probe's own Rc, for timers.
    me: std::rc::Weak<RefCell<Probe>>,
}

impl Probe {
    fn pv(&self, n: u16) -> f64 {
        self.runv.get(&n).or_else(|| self.defaults.get(&n)).copied().unwrap_or(0.0)
    }

    fn targets(&self) -> Vec<Target> {
        match self.shape {
            "block" => {
                let ctr = if self.axes == "XY" { "STOCK CENTER".to_string() } else { format!("CENTER IN {}", self.axes) };
                // The block is 200 × 160 at (100, 70). Edge bars sit on the
                // edge lines, between the corner spots, and are thick enough
                // to tap (EDGE_T).
                const T: f64 = EDGE_T;
                vec![
                    corner("c-bl", 100.0, 70.0, "BACK LEFT CORNER"),
                    corner("c-br", 300.0, 70.0, "BACK RIGHT CORNER"),
                    corner("c-fl", 100.0, 230.0, "FRONT LEFT CORNER"),
                    corner("c-fr", 300.0, 230.0, "FRONT RIGHT CORNER"),
                    edge("e-b", 136.0, 70.0 - T / 2.0, 128.0, T, "BACK EDGE"),
                    edge("e-f", 136.0, 230.0 - T / 2.0, 128.0, T, "FRONT EDGE"),
                    edge("e-l", 100.0 - T / 2.0, 106.0, T, 88.0, "LEFT EDGE"),
                    edge("e-r", 300.0 - T / 2.0, 106.0, T, 88.0, "RIGHT EDGE"),
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
            "top" => vec![center("top", 200.0, 150.0, "TOP FACE · SETS Z")],
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

    /// Whether the picture's spots pick the target (several real places on
    /// the part). Other shapes have one target, or pick it in the option rows.
    fn picks_on_picture(&self) -> bool {
        matches!(self.shape, "block" | "pocket")
    }

    /// The values a routine depends on; the rest are under MORE. Together
    /// with `locals` there are never more than three, so the screen keeps
    /// three fixed slots.
    fn key_params(&self, kind: &str) -> Vec<u16> {
        match kind {
            "corner" | "icorner" => vec![3023, 3019, 3018],
            "edge" if self.angle => vec![3024, 3023, 3019],
            "edge" => vec![3019, 3018],
            "stock" => vec![3019, 3018],
            "two" => vec![3019, 3018],
            "top" => vec![3020, 3021],
            "boss" => vec![3025, 3019, 3018],
            "pocket" => vec![3026, 3027, 3018],
            "bore" => vec![3025, 3018],
            "slot" => vec![if self.axes == "Y" { 3029 } else { 3028 }, 3019],
            "cyl" | "flat" | "dia" | "runout" => vec![3021, 3019],
            // Two operator inputs (locals) take the other slots; XY clearance is under MORE.
            "defl" => vec![3021],
            "setter" | "nose" => vec![3005, 3009],
            "ring" => vec![3033],
            _ => vec![],
        }
    }

    fn used_params(&self, kind: &str) -> Vec<u16> {
        match kind {
            "cyl" | "flat" | "dia" | "runout" | "defl" => vec![3019, 3021, 3016, 3015],
            "setter" | "nose" => vec![3004, 3005, 3006, 3007, 3009],
            "top" => vec![3020, 3021, 3016, 3015, 3017],
            "ring" => vec![3033, 3016, 3015, 3017, 3023, 3020, 3021],
            _ => vec![3023, 3019, 3021, 3018, 3020, 3022, 3016, 3015, 3017],
        }
    }

    /// The two option rows under the picture: (label, choices) each.
    fn options(&self, sel: &Target) -> (&'static str, Vec<ProbeOpt>, &'static str, Vec<ProbeOpt>) {
        let it = |id: &str, label: &str, on: bool| ProbeOpt { id: id.into(), label: label.into(), on, enabled: true };
        match (self.shape, sel.kind) {
            (_, "edge") if self.mode == "find" => ("EDGE ANGLE", vec![it("angle-off", "NO", !self.angle), it("angle-on", "MEASURE IT TOO", self.angle)], "", vec![]),
            ("block", "stock" | "two") => {
                let one = if self.both { "" } else { side_axis(self.start) };
                let axes = vec![it("both", "X AND Y", self.both), it("axis-X", "X ONLY", one == "X"), it("axis-Y", "Y ONLY", one == "Y")];
                let sides = ['F', 'B', 'L', 'R']
                    .iter()
                    .map(|k| ProbeOpt {
                        id: format!("start-{k}").into(),
                        label: side_name(*k).into(),
                        on: *k == self.start,
                        enabled: self.both || side_axis(*k) == one,
                    })
                    .collect();
                ("MEASURE", axes, "FIRST SIDE", sides)
            }
            ("slot", _) => (
                "TYPE",
                vec![it("ridge", "RIDGE", self.ridge), it("valley", "SLOT", !self.ridge)],
                "ACROSS",
                vec![it("X", "X", self.axes != "Y"), it("Y", "Y", self.axes == "Y")],
            ),
            ("cyl", _) => ("FIND", vec![it("tgt-ctr", "CENTERLINE Y, Z", sel.id == "ctr"), it("tgt-flat", "A0 ON A FLAT", sel.id == "flat")], "", vec![]),
            _ => ("", vec![], "", vec![]),
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
        self.locv.get(&i).copied().unwrap_or_else(|| locals(kind).get(i).map_or(0.0, |l| l.default()))
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
            "top" => ("o<probe_z_minus_wco> call".into(), None),
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
                    (format!("o<probe_auto_stock_center> call [{s}] [{}] [{}]", units().fmt(self.local("stock", 0)), self.token), Some("stock-measure"))
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
            "cyl" => (format!("o<probe_a_setup_cylinder_center> call [{}] [1] [0]", units().fmt(self.local("cyl", 0))), None),
            "flat" => ("o<probe_a_rotary> call [1]".into(), None),
            "dia" => (format!("o<probe_a_measure_diameter> call [{}] [1]", units().fmt(self.local("dia", 0))), None),
            "runout" => (format!("o<probe_a_measure_runout> call [{}] [1]", units().fmt(self.local("runout", 0))), None),
            "defl" => (format!("o<probe_a_measure_deflection> call [{}] [{}] [1]", units().fmt(self.local("defl", 0)), units().fmt(self.local("defl", 1))), None),
            "ring" => ("o<probe_cal_round_pocket> call".into(), None),
            "setter" => ("o<tool_touch_off> call".into(), None),
            "nose" => ("o<probe_spindle_nose> call".into(), None),
            k => return Err(format!("No routine for {k}")),
        })
    }

    /// What is wrong with the tool in the spindle for `t`, if anything.
    fn tool_problem(&self, t: &Target, tool: i32) -> Option<String> {
        match Self::needs(t.kind) {
            Needs::Probe if tool != self.probe_tool => Some(format!("Load the probe (T{}) first", self.probe_tool)),
            Needs::Tool if tool == self.probe_tool || tool <= 0 => Some("Load the tool to measure (not the probe)".into()),
            Needs::Empty if tool != 0 => Some("Unload the spindle (T0) before probing the nose".into()),
            _ => None,
        }
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
        if let Some(why) = self.tool_problem(t, s.tool_in_spindle) {
            return Err(why);
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
        if ["cyl", "flat", "dia", "runout", "defl"].contains(&t.kind) && !s.has_axis(machine::Axis::A) {
            return Err("The A axis is not configured".into());
        }
        Ok(())
    }

    fn active(&self) -> usize {
        self.status.as_ref().map_or(0, |s| s.wcs.index().unwrap_or(0))
    }

    /// The routine name a job op uses for `t`, when a job can run it:
    /// FIND targets that need no operator step part way.
    fn job_routine(&self, t: &Target) -> Option<&'static str> {
        if self.mode != "find" {
            return None;
        }
        Some(match t.kind {
            "stock" | "corner" | "icorner" | "top" | "boss" | "bore" | "pocket" => t.kind,
            "edge" if !self.angle => "edge",
            "slot" if self.ridge => "ridge",
            "slot" => "slot",
            _ => return None,
        })
    }

    /// Whether the selected target can be checked (MEASURE) in a job.
    fn job_measurable(&self) -> bool {
        let t = self.selected();
        self.job_routine(&t).is_some_and(|r| !jobcore::probe::values_for(r).is_empty())
    }

    /// A job op for the selected target, started from where the probe is
    /// now: (name, params for jobcore's probe op). `measure` makes a check
    /// that reports instead of writing the work offset.
    fn job_op(&self, measure: bool) -> Result<(String, serde_json::Value), String> {
        let t = self.selected();
        let routine = self.job_routine(&t).ok_or("A job can run FIND routines that need no jogging part way (not TWO FACES, angles, rotary or calibration)")?;
        if measure && jobcore::probe::values_for(routine).is_empty() {
            return Err("A check measures a bore, boss, pocket, slot, ridge or top face".into());
        }
        let s = self.status.as_ref().ok_or("Not connected to LinuxCNC")?;
        if !s.all_homed() {
            return Err("Home the machine first".into());
        }
        if s.tool_in_spindle != self.probe_tool {
            return Err(format!("Load the probe (T{}) and jog it to where the routine starts", self.probe_tool));
        }
        // The op goes up to the top of Z travel and comes back down to here.
        let mi = crate::machine_info::get();
        if s.actual[2] > mi.z_top - mi.mm(5.0) {
            return Err("Jog the probe down to where the routine starts first: a job step starts from there".into());
        }
        let target = self.target_wcs();
        if target != self.active() {
            return Err(format!("Make {} active first: the start is taught in it", WCS[target]));
        }
        let calls = match routine {
            "stock" => {
                let side = match self.start { 'L' => 0, 'R' => 1, 'F' => 2, _ => 3 };
                vec![
                    format!("o<probe_auto_stock_center> call [{side}] [{}] [1]", units().fmt(self.local("stock", 0))),
                    "o<probe_auto_stock_apply> call [1]".to_string(),
                ]
            }
            _ => vec![self.plain_call(&t).ok_or("No routine for this target")?],
        };
        // Every routine checks #3015-#3022 (o<millgui_probe_ready>), so the
        // op carries them all and does not depend on millgui having written
        // its settings to LinuxCNC before the job runs.
        let mut nums: Vec<u16> = (3015..=3022).collect();
        nums.extend(self.used_params(t.kind).into_iter().filter(|n| *n != 3030 && *n != 3014 && !(3015..=3022).contains(n)));
        let values: serde_json::Map<String, serde_json::Value> = nums.into_iter().map(|n| (n.to_string(), serde_json::json!(self.pv(n)))).collect();
        let w = s.work_position();
        // To the machine's decimals (3 in mm, 4 in inches).
        let r3 = |v: f64| mi.units.round(v);
        let label = t.label.clone();
        // "TOP FACE · SETS Z" checks as "CHECK TOP FACE": a check sets nothing.
        let name = if measure { format!("CHECK {}", label.split(" · ").next().unwrap_or(&label)) } else { label.clone() };
        let mut params = serde_json::json!({
            "mode": if measure { "measure" } else { "find" },
            "routine": routine,
            "label": label,
            "calls": calls,
            "values": values,
            "wcs": WCS[target],
            "start": [r3(w[0]), r3(w[1]), r3(w[2])],
        });
        if !measure {
            // A FIND sets the work offset, so its start is kept in machine
            // coordinates: the same spot on every run.
            params["start_machine"] = serde_json::json!([r3(s.actual[0]), r3(s.actual[1]), r3(s.actual[2])]);
        }
        Ok((name, params))
    }

    /// The macro call for a target with no multi-step flow (`call` without
    /// the flows; both must name the same routines).
    fn plain_call(&self, t: &Target) -> Option<String> {
        let side = |id: &str| match id {
            "c-fl" | "i-fl" => "front_left",
            "c-fr" | "i-fr" => "front_right",
            "c-bl" | "i-bl" => "back_left",
            _ => "back_right",
        };
        Some(match t.kind {
            "corner" => format!("o<probe_{}_top_corner> call", side(t.id)),
            "icorner" => format!("o<probe_{}_inside_corner> call", side(t.id)),
            "edge" if !self.angle => {
                let m = match t.id { "e-f" => "y_plus", "e-b" => "y_minus", "e-l" => "x_plus", _ => "x_minus" };
                format!("o<probe_{m}_wco> call")
            }
            "top" => "o<probe_z_minus_wco> call".into(),
            "boss" => "o<probe_round_boss> call".into(),
            "pocket" => "o<probe_rect_pocket_center_start> call".into(),
            "bore" => "o<probe_round_pocket_center_start> call".into(),
            "slot" => {
                let a = if self.axes == "Y" { "y" } else { "x" };
                if self.ridge { format!("o<probe_ridge_{a}> call") } else { format!("o<probe_valley_{a}_center_start> call") }
            }
            _ => return None,
        })
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
        // FIND writes the work offset; MEASURE and CAL only report (the
        // ring calibration would otherwise move the active work zero).
        if Self::needs(t.kind) == Needs::Probe {
            run.insert(3030, if self.mode == "find" { 0.0 } else { 1.0 });
        }
        run.retain(|n, v| self.defaults.get(n) != Some(v));
        // Defaults LinuxCNC does not hold yet (the quiet-time write has not
        // happened): the whole set first.
        if self.restore_pending {
            steps.extend(Self::assign_lines(self.defaults.iter().map(|(n, v)| (*n, *v))));
            self.restore_pending = false;
            self.state.set("probe.restore-pending", 0);
        }
        if !run.is_empty() {
            steps.extend(Self::assign_lines(run.iter().map(|(n, v)| (*n, *v))));
            restore.extend(Self::assign_lines(run.keys().map(|n| (*n, self.defaults.get(n).copied().unwrap_or(0.0)))));
            self.mark_dirty(true);
        }
        steps.push(call);
        // mymill.hal latches "nose reference valid" from digital out 7;
        // millgui's own nose macro uses no HAL, so millgui pulses it after a
        // nose run that succeeded (only where that latch exists).
        if t.kind == "nose" && self.io.as_ref().is_some_and(|io| io.read().nose_reference.is_some()) {
            steps.extend(["M64 P7", "G4 P0.1", "M65 P7"].map(String::from));
        }
        if target != active {
            restore.push(WCS[active].to_string());
        }
        // Several stock macros (bore, pocket, slot) end in G91. Left on, the
        // next G53 or absolute move from MDI or RUN FROM SELECTED fails or
        // goes somewhere else, so put back the mode the probing started in.
        if let Some(mode) = distance_mode(self.status.as_ref()) {
            restore.push(mode.to_string());
        }
        self.results.clear();
        let what = self.selected().label;
        self.result_text = format!("Running: {what}");
        self.run = Some(Run { steps, restore, sent: None, failed: None, restoring: false, restore_failed: false, what, kind: t.kind, flow_step, not_before: None, done_at: None });
        self.pump();
        Ok(())
    }

    fn stop(&mut self) {
        self.machine.send(Command::Abort);
        self.stopped();
    }

    /// The bar's STOP aborted LinuxCNC: a run from this screen ends here
    /// (no further step is sent) and its defaults go back.
    pub fn stopped(&mut self) {
        if let Some(r) = &mut self.run {
            if !r.restoring {
                r.failed.get_or_insert_with(|| "Stopped".into());
                r.steps.clear();
                r.sent = None;
                r.not_before = Some(Instant::now() + ABORT_SETTLE);
            }
        }
        self.refresh();
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
                    run.restore_failed |= run.restoring;
                }
            }
            run.sent = None;
        }
        if run.failed.is_some() && !run.restoring {
            run.steps.clear();
        }
        // After a STOP: wait for the abort to be over (the 100 ms timer
        // comes back here).
        if run.not_before.is_some_and(|t| Instant::now() < t) || (run.not_before.is_some() && s.busy()) {
            return;
        }
        run.not_before = None;
        if run.steps.is_empty() && !run.restoring {
            run.restoring = true;
            run.steps = std::mem::take(&mut run.restore);
        }
        if run.steps.is_empty() {
            let done = *run.done_at.get_or_insert_with(Instant::now);
            if done.elapsed() < RESULT_GRACE {
                // Status only arrives on a change, so come back by timer.
                let me = self.me.clone();
                slint::Timer::single_shot(RESULT_GRACE, move || {
                    if let Some(p) = me.upgrade() {
                        let mut p = p.borrow_mut();
                        p.pump();
                        p.refresh();
                    }
                });
                return;
            }
            let run = self.run.take().unwrap();
            self.finish(run);
            return;
        }
        let line = run.steps.remove(0);
        run.sent = Some((Instant::now(), s.echo_serial, false));
        self.machine.send(Command::Mdi(line));
    }

    fn finish(&mut self, run: Run) {
        // Keep the marker until the defaults are really back; try again later.
        self.mark_dirty(run.restore_failed);
        if run.restore_failed {
            self.restore_after = Some(Instant::now() + RESTORE_RETRY);
        }
        let results = std::mem::take(&mut self.results);
        if let Some(e) = &run.failed {
            self.result_text = if e == "Stopped" { format!("{}: stopped", run.what) } else { format!("{} failed: {e}", run.what) };
            self.flow = None;
            self.note(format!("Probe routine failed: {e}"), true);
            return;
        }
        // Putting the defaults back measures nothing: the last result stays.
        if run.kind.is_empty() {
            return;
        }
        let num = |name: &str| results.iter().find(|(n, _)| n == name).and_then(|(_, v)| v.first().copied());
        let mut lines: Vec<String> = Vec::new();
        match run.flow_step {
            Some("stock-measure") => {
                if let Some((_, v)) = results.iter().find(|(n, _)| n == "auto_stock_panel.receive_result") {
                    if v.len() == 9 && v[0] as u32 == self.token {
                        self.flow = Some(Flow::StockMeasured { token: self.token, center: (v[7], v[8]), size: (v[5], v[6]) });
                        lines.push(format!("Stock {} × {}", fmt3(v[5]), fmt3(v[6])));
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
                        lines.push(format!("First face at {}", fmt3(v[2])));
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
                        lines.push(format!("Width {}, center {}", fmt3(v[4]), fmt3(v[5])));
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
        self.result_text = lines.join("   ·   ");
        self.note(format!("{}: done", run.what), false);
    }

    /// A macro's `(DEBUG, EVAL[...])` line. Returns true when it was one.
    fn message(&mut self, m: &Message) -> bool {
        let Some((name, args)) = parse_eval(&m.text) else {
            if m.kind == MessageKind::Error {
                if let Some(r) = &mut self.run {
                    r.failed.get_or_insert_with(|| m.text.trim().to_string());
                    r.restore_failed |= r.restoring;
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
                    if let Err(e) = self.save_toml(n, *v) {
                        self.note(format!("Not saved to millgui.toml: {e}"), true);
                    }
                }
            }
        }
        // Kept only for a routine this page started: a job's probing
        // steps print the same lines.
        if self.run.is_some() {
            self.results.push((name, args));
        }
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
        crate::models::sync(live.get_pr_shapes(), items, |m| live.set_pr_shapes(m));
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
        let on_picture = self.picks_on_picture();
        live.set_pr_hint(
            match sel.kind {
                _ if on_picture => "TAP A BLUE SPOT TO CHOOSE WHERE",
                "cyl" | "flat" | "dia" | "runout" | "defl" => "END VIEW · LOOKING ALONG X · TABLE BELOW",
                "top" | "setter" | "nose" => "SIDE VIEW",
                _ => "TOP VIEW",
            }
            .into(),
        );
        // Picture: the spots are the only controls on it.
        let (outline, arrows, heads) = picture(self, &sel);
        live.set_pr_outline(outline.into());
        live.set_pr_arrows(arrows.into());
        live.set_pr_heads(heads.into());
        let moves = segs(self, &sel);
        crate::probe_anim::play(
            ui,
            crate::probe_anim::script(&crate::probe_anim::Routine {
                kind: sel.kind,
                segs: &moves,
                at: (sel.x, sel.y),
                angle: self.angle,
                ridge: self.ridge,
                side: side_name(self.start),
            }),
        );
        let spots: Vec<ProbeSpot> = if on_picture {
            targets
                .iter()
                .map(|t| ProbeSpot {
                    id: t.id.into(),
                    x: t.x as f32,
                    y: t.y as f32,
                    w: t.w as f32,
                    h: t.h as f32,
                    r: if t.id == "ctr" { CENTER_R } else { CORNER_R } as f32,
                    on: t.id == sel.id,
                })
                .collect()
        } else {
            Vec::new()
        };
        crate::models::sync(live.get_pr_spots(), spots, |m| live.set_pr_spots(m));
        // Choices for the target: two labelled groups, always the same
        // buttons for a target (a choice that doesn't apply is disabled, not
        // removed), so nothing moves under the finger.
        let (l1, o1, l2, o2) = self.options(&sel);
        live.set_pr_opts_label(l1.into());
        crate::models::sync(live.get_pr_opts(), o1, |m| live.set_pr_opts(m));
        live.set_pr_opts2_label(l2.into());
        crate::models::sync(live.get_pr_opts2(), o2, |m| live.set_pr_opts2(m));
        // Values
        let key = self.key_params(sel.kind);
        let row = |n: u16| {
            let p = param(n);
            ProbeParam {
                id: n.to_string().into(),
                label: p.map_or("", |p| p.label).into(),
                value: fmt_value(n, self.pv(n)).into(),
                unit: p.map_or("", |p| unit_of(p.unit, units())).into(),
                changed: self.runv.contains_key(&n),
                def: fmt_value(n, self.defaults.get(&n).copied().unwrap_or(0.0)).into(),
            }
        };
        let mut fields: Vec<ProbeParam> = key.iter().map(|n| row(*n)).collect();
        let n_key = fields.len();
        for (i, l) in locals(sel.kind).iter().enumerate() {
            fields.push(ProbeParam {
                id: format!("L{i}").into(),
                label: l.label.into(),
                value: fmt_value(0, self.local(sel.kind, i)).into(),
                unit: units().name().into(),
                changed: self.locv.contains_key(&i),
                def: fmt_value(0, l.default()).into(),
            });
        }
        // Each value keeps its slot across the shape's targets and options,
        // so picking another target never moves a value that stays.
        let ids = |kind: &str, angle: bool| -> Vec<String> {
            let mut v: Vec<String> = if kind == "edge" {
                if angle { vec![3024, 3023, 3019] } else { vec![3019, 3018] }.iter().map(u16::to_string).collect()
            } else {
                self.key_params(kind).iter().map(u16::to_string).collect()
            };
            v.extend((0..locals(kind).len()).map(|i| format!("L{i}:{kind}")));
            v
        };
        let mut lists: Vec<Vec<String>> = Vec::new();
        for t in &targets {
            lists.push(ids(t.kind, false));
            if t.kind == "edge" {
                lists.push(ids(t.kind, true));
            }
        }
        let want = ids(sel.kind, sel.kind == "edge" && self.angle);
        let slot_of = slots(&lists, &want);
        let mut placed: Vec<ProbeParam> = (0..3).map(|_| ProbeParam::default()).collect();
        // ALL SETTINGS: what the routine asks for (as on the screen, the
        // operator's own inputs first), then every other value it reads.
        let mut more: Vec<ProbeParam> = fields[n_key..].to_vec();
        more.extend(fields[..n_key].iter().cloned());
        more.extend(self.used_params(sel.kind).into_iter().filter(|n| !key.contains(n)).map(row));
        for (f, slot) in fields.into_iter().zip(slot_of) {
            placed[slot] = f;
        }
        crate::models::sync(live.get_pr_params(), placed, |m| live.set_pr_params(m));
        crate::models::sync(live.get_pr_more(), more, |m| live.set_pr_more(m));
        live.set_pr_changed(!self.runv.is_empty() || !self.locv.is_empty());
        // #3014 is [probe]tool (SETUP), not a default to edit here.
        crate::models::sync(live.get_pr_defaults(), PARAMS.iter().filter(|p| p.n != crate::probe_defaults::PROBE_TOOL).map(|p| {
            let mut r = row(p.n);
            r.value = fmt_value(p.n, self.defaults.get(&p.n).copied().unwrap_or(0.0)).into();
            r.changed = false;
            r.def = p.group.into();
            r
        }).collect::<Vec<_>>(), |m| live.set_pr_defaults(m));
        // Routine note and buttons
        let what = match sel.kind {
            "corner" => "outside top corner (touches top Z first)".to_string(),
            "icorner" => "inside corner".into(),
            "edge" => format!("single edge{}", if self.angle { " + edge angle (touches top Z)" } else { "" }),
            "stock" => "Auto Stock: walks the outside, then APPLY".into(),
            "two" => "Two Faces: first face, opposite face, APPLY".into(),
            "top" => "Z down onto the top face; start above it".into(),
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
        let mut what = what;
        if let Some(c) = what.get(..1) {
            what.replace_range(..1, &c.to_uppercase());
        }
        live.set_pr_routine(what.into());
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
        let blocked = self.check(&sel).err();
        // The wrong tool is the only thing in the way: offer the change
        // in place of the reason.
        live.set_pr_load_offer(!ok && needs != Needs::Tool && blocked.is_some() && blocked == self.tool_problem(&sel, tool));
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
        live.set_pr_ready(blocked.unwrap_or_default().into());
        live.set_pr_job_ok(self.job_routine(&sel).is_some());
        live.set_pr_job_check(self.job_measurable());
    }

    /// The job op for the selected setup (see `job_op`).
    pub fn job_step(&self, measure: bool) -> Result<(String, serde_json::Value), String> {
        self.job_op(measure)
    }

    /// A job with probing ops is starting: put the probe defaults back once
    /// it is idle again, as after a run from this screen.
    pub fn job_started(&mut self) {
        // Armed until the program is seen running, so the restore can't
        // slip in between CYCLE START and the program starting.
        self.job_armed = true;
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

    /// A choice from the option rows under the picture.
    fn option(&mut self, o: &str) {
        match o {
            "angle-on" => self.angle = true,
            "angle-off" => self.angle = false,
            "both" => self.both = true,
            // One axis: keep the first side if it is on that axis.
            "axis-X" | "axis-Y" => {
                let axis = &o[5..];
                self.both = false;
                if side_axis(self.start) != axis {
                    self.start = if axis == "X" { 'L' } else { 'F' };
                }
            }
            s if s.starts_with("start-") => {
                let k = s.chars().nth(6).unwrap_or('F');
                // With one axis only that axis's sides are offered.
                if self.both || side_axis(k) == side_axis(self.start) {
                    self.start = k;
                }
            }
            s if s.starts_with("tgt-") => {
                if let Some(t) = self.targets().into_iter().find(|t| t.id == &s[4..]) {
                    self.tgt = t.id;
                }
            }
            "ridge" => self.ridge = true,
            "valley" => self.ridge = false,
            "X" => self.axes = "X",
            "Y" => self.axes = "Y",
            _ => {}
        }
        self.flow = None;
        self.settle();
    }

    fn set_value(&mut self, id: &str, v: f64) {
        // To the machine's decimals (3 in mm, 4 in inches).
        let v = units().round(v);
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
        let saved = self.save_toml(n, v);
        self.machine.send(Command::Mdi(format!("#{n}={v:.6}")));
        self.defaults.insert(n, v);
        self.runv.remove(&n);
        self.save_defaults();
        saved
    }

    /// Save an owned default to millgui.toml (not in the simulator).
    fn save_toml(&mut self, n: u16, v: f64) -> Result<(), String> {
        let Some(d) = crate::probe_defaults::def(n) else { return Ok(()) };
        self.sources.insert(n, crate::probe_defaults::Source::Toml);
        let Some((path, seed)) = &self.toml else { return Ok(()) };
        crate::machine_cfg::write_key(path, seed.as_deref(), d.key, Some(toml_edit::value((v * 1e6).round() / 1e6)))
    }

    /// SETUP changed a default (already saved): use it from the next run on,
    /// and write it to LinuxCNC once it is quiet.
    pub fn adopt(&mut self, n: u16, v: f64) {
        self.defaults.insert(n, v);
        self.sources.insert(n, crate::probe_defaults::Source::Toml);
        self.runv.remove(&n);
        self.save_defaults();
        self.mark_dirty(true);
        self.restore_after = None;
        // The operator is on SETUP, not jogging: no need to wait.
        self.maybe_restore(true);
        self.pump();
    }

    /// Defaults LinuxCNC may not hold (a crash mid-run, values new to it):
    /// write them once LinuxCNC takes MDI (on and homed) and nothing has
    /// moved for QUIET_FOR. Homing and jogs leave the interpreter idle, and
    /// an MDI then would abort them (or be refused mid-jog); the wait keeps
    /// it out of a burst of jogs.
    fn maybe_restore(&mut self, now: bool) {
        let Some(s) = &self.status else { return };
        let quiet = s.power_on() && s.all_homed() && !s.busy() && s.inpos && !s.homing.iter().any(|h| *h);
        if !quiet {
            self.quiet_since = None;
            return;
        }
        let since = *self.quiet_since.get_or_insert_with(Instant::now);
        let settled = now || since.elapsed() >= QUIET_FOR;
        let due = self.restore_after.is_none_or(|t| Instant::now() >= t);
        if self.restore_pending && self.run.is_none() && settled && due {
            let lines = Self::assign_lines(self.defaults.iter().map(|(n, v)| (*n, *v)));
            self.run = Some(Run { steps: Vec::new(), restore: lines, sent: None, failed: None, restoring: false, restore_failed: false, what: "Restore probe defaults".into(), kind: "", flow_step: None, not_before: None, done_at: None });
            self.restore_pending = false;
            self.restore_after = None;
        }
    }

    /// The defaults as MDI lines, when LinuxCNC may not hold them yet (the
    /// quiet-time write has not happened): for another screen to send ahead
    /// of a macro that reads them (TOOLS › MEASURE / CHECK). Taking them
    /// clears the pending write.
    pub fn take_pending_defaults(&mut self) -> Vec<String> {
        if !self.restore_pending || self.run.is_some() {
            return Vec::new();
        }
        self.restore_pending = false;
        self.state.set("probe.restore-pending", 0);
        Self::assign_lines(self.defaults.iter().map(|(n, v)| (*n, *v)))
    }

    /// An owned default as millgui uses it now, and where it came from.
    pub fn default_of(&self, n: u16) -> (Option<f64>, crate::probe_defaults::Source) {
        let src = self.sources.get(&n).copied().unwrap_or(crate::probe_defaults::Source::Missing);
        (if src == crate::probe_defaults::Source::Missing { None } else { self.defaults.get(&n).copied() }, src)
    }
}

/// A result to the machine's decimals (3 in mm, 4 in inches).
fn fmt3(v: f64) -> String {
    units().fmt(v)
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
        // Seen from the front: the stock and the face the probe comes down on.
        "top" => rect(80.0, 170.0, 240.0, 90.0),
        "round" => circle(200.0, 150.0, 80.0),
        "bore" => rect(60.0, 40.0, 280.0, 220.0) + &circle(200.0, 150.0, 60.0),
        "slot" if p.axes == "Y" => rect(60.0, 110.0, 280.0, 80.0),
        "slot" => rect(160.0, 40.0, 80.0, 220.0),
        // The rotary from its end: the axis marks; the part (or its index
        // mark) turns with A and is drawn by the preview (`pr-part`).
        "cyl" | "dia" | "runout" | "defl" => {
            let marks = "M 200 80 L 200 88 M 200 242 L 200 250 M 108 165 L 122 165 M 278 165 L 292 165 ";
            if sel.kind == "flat" { marks.to_string() } else { circle(200.0, 165.0, 70.0) + marks }
        }
        "ring" => circle(200.0, 150.0, 100.0) + &circle(200.0, 150.0, 66.0),
        "setter" => rect(160.0, 200.0, 80.0, 40.0),
        _ => rect(160.0, 200.0, 80.0, 40.0) + &rect(170.0, 60.0, 60.0, 70.0),
    };
    let mut lines = String::new();
    let mut heads = String::new();
    // Walks that start on a chosen side mark the first touch with a bigger head.
    let first_big = matches!(sel.kind, "stock" | "two");
    for (i, [x1, y1, x2, y2]) in segs(p, sel).into_iter().enumerate() {
        lines += &format!("M {x1:.1} {y1:.1} L {x2:.1} {y2:.1} ");
        let (dx, dy) = (x2 - x1, y2 - y1);
        let l = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (ux, uy) = (dx / l, dy / l);
        let k = if first_big && i == 0 { 2.0 } else { 1.0 };
        let (bx, by) = (x2 - ux * 12.0 * k, y2 - uy * 12.0 * k);
        let w = 6.0 * k;
        heads += &format!("M {x2:.1} {y2:.1} L {:.1} {:.1} L {:.1} {:.1} Z ", bx - uy * w, by + ux * w, bx + uy * w, by - ux * w);
    }
    (outline, lines, heads)
}

/// Slots (0–2) for the values `want`, chosen so that across `lists` (the
/// values each target of the shape shows) a value always gets the same slot
/// and no two values shown together share one. Falls back to list order if
/// the values cannot be placed that way.
fn slots(lists: &[Vec<String>], want: &[String]) -> Vec<usize> {
    let mut all = lists.to_vec();
    all.push(want.to_vec());
    let mut slot: BTreeMap<String, usize> = BTreeMap::new();
    for l in &all {
        for id in l {
            if slot.contains_key(id) {
                continue;
            }
            // Slots taken by values that show alongside this one.
            let taken: Vec<usize> =
                all.iter().filter(|m| m.contains(id)).flat_map(|m| m.iter().filter_map(|o| slot.get(o).copied())).collect();
            let Some(free) = (0..3).find(|s| !taken.contains(s)) else { return (0..want.len()).collect() };
            slot.insert(id.clone(), free);
        }
    }
    let out: Vec<usize> = want.iter().map(|id| slot[id]).collect();
    let mut seen = out.clone();
    seen.sort();
    seen.dedup();
    if seen.len() == out.len() { out } else { (0..want.len()).collect() }
}

/// Probe approach moves in picture units (from the mockup).
fn segs(p: &Probe, t: &Target) -> Vec<[f64; 4]> {
    let k = t.kind;
    if k == "corner" || k == "icorner" {
        let dx = if t.x < 200.0 { -1.0 } else { 1.0 };
        let dy = if t.y < 150.0 { -1.0 } else { 1.0 };
        let (x, y) = (t.x, t.y);
        return if k == "icorner" {
            // Both walls from one spot inside the pocket.
            vec![[x - dx * 40.0, y - dy * 40.0, x, y - dy * 40.0], [x - dx * 40.0, y - dy * 40.0, x - dx * 40.0, y]]
        } else {
            vec![[x + dx * 60.0, y - dy * 30.0, x, y - dy * 30.0], [x - dx * 30.0, y + dy * 60.0, x - dx * 30.0, y]]
        };
    }
    if k == "edge" {
        // From outside the block to its edge line (70/230, 100/300); with
        // the angle, a second touch further along the edge.
        let (a, b) = match t.id {
            "e-b" => ([200.0, 6.0, 200.0, 70.0], [160.0, 6.0, 160.0, 70.0]),
            "e-f" => ([200.0, 294.0, 200.0, 230.0], [240.0, 294.0, 240.0, 230.0]),
            "e-l" => ([14.0, 150.0, 100.0, 150.0], [14.0, 185.0, 100.0, 185.0]),
            _ => ([386.0, 150.0, 300.0, 150.0], [386.0, 115.0, 300.0, 115.0]),
        };
        return if p.angle { vec![a, b] } else { vec![a] };
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
        "cyl" | "dia" | "defl" => vec![[200.0, 40.0, 200.0, 95.0], [80.0, 165.0, 130.0, 165.0], [320.0, 165.0, 270.0, 165.0]],
        "runout" => vec![[200.0, 40.0, 200.0, 95.0]],
        "top" => vec![[200.0, 50.0, 200.0, 170.0]],
        "flat" => vec![[170.0, 40.0, 170.0, crate::probe_anim::flat_y(170.0)], [230.0, 40.0, 230.0, crate::probe_anim::flat_y(230.0)]],
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
    use crate::probe_defaults as pd;
    let mc = crate::machine_cfg::get();
    let probe_tool = mc.probe_tool(cfg);
    // What LinuxCNC holds: the parameter file, with the saved copy on top
    // when a run never restored the defaults.
    let state = if sim { Settings::memory() } else { Settings::at("probe") };
    let mut restore_pending = state.get("probe.restore-pending") == Some("1");
    let file = pd::param_file(cfg);
    let mut base = file.clone();
    if restore_pending {
        for v in state.list("probe.default.") {
            if let Some((n, v)) = v.split_once('=') {
                if let (Ok(n), Ok(v)) = (n.parse(), v.parse()) {
                    base.insert(n, v);
                }
            }
        }
    }
    // millgui's own values on top: millgui.toml, else those, else seeds.
    let mut defaults = base.clone();
    let mut sources = BTreeMap::new();
    for (n, (v, src)) in pd::resolve(mc, &base, crate::machine_info::get().units.inch()) {
        if let Some(v) = v {
            defaults.insert(n, v);
        }
        sources.insert(n, src);
    }
    defaults.insert(pd::PROBE_TOOL, probe_tool as f64);
    // Anything LinuxCNC does not already hold goes to it once it is quiet.
    let used = mc.probe.enabled != Some(false) || mc.toolsetter.enabled == Some(true);
    if used && defaults.iter().any(|(n, v)| (sources.contains_key(n) || *n == pd::PROBE_TOOL) && file.get(n) != Some(v)) {
        restore_pending = true;
    }
    let toml = (!sim).then(|| mc.target(cfg));
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
        restore_after: None,
        quiet_since: None,
        job_armed: false,
        probe_tool,
        sources,
        toml,
        machine: machine.clone(),
        io,
        ui: ui.as_weak(),
        rotation,
        me: std::rc::Weak::new(),
    }));
    probe.borrow_mut().me = Rc::downgrade(&probe);
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
        if let Some(t) = p.targets().into_iter().find(|t| t.id == id.as_str()) {
            p.tgt = t.id;
        }
        p.flow = None;
        p.settle();
        p.refresh();
    });
    let p = probe.clone();
    actions.on_probe_opt(move |o| {
        let mut p = p.borrow_mut();
        p.option(&o);
        p.refresh();
    });
    let p = probe.clone();
    actions.on_probe_step(move |id, dir| {
        let mut p = p.borrow_mut();
        let u = units();
        let step = id.parse::<u16>().map_or(u.pick(0.5, 0.02), |n| step_of(n, u));
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

/// "G90" or "G91" from the active modal codes (stored as code * 10).
fn distance_mode(s: Option<&Status>) -> Option<&'static str> {
    let g = &s?.active_gcodes;
    if g.contains(&910) {
        Some("G91")
    } else if g.contains(&900) {
        Some("G90")
    } else {
        None
    }
}

pub fn apply_status(ui: &MainWindow, probe: &Rc<RefCell<Probe>>, s: &Status) {
    let mut p = probe.borrow_mut();
    // Everything `check` reads, so the readiness hint never goes stale.
    let key = |o: &Status| (o.tool_in_spindle, o.power_on(), o.busy(), o.wcs, o.all_homed(), o.active_gcodes.contains(&400), o.g92_offset.iter().any(|v| *v != 0.0));
    let was = p.status.as_ref().map(key);
    p.status = Some(s.clone());
    if p.job_armed && s.busy() {
        p.job_armed = false;
        p.mark_dirty(true);
    }
    p.maybe_restore(false);
    p.pump();
    if was != Some(key(s)) || p.run.is_some() {
        p.show(ui);
    }
}

/// Steps finish without a status change (an instant MDI), so poll too.
pub fn start_timer(probe: &Rc<RefCell<Probe>>) -> slint::Timer {
    let timer = slint::Timer::default();
    let p = probe.clone();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        let mut p = p.borrow_mut();
        if p.restore_pending && p.run.is_none() {
            p.maybe_restore(false);
        }
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
    fn distance_mode_reads_g90_g91_not_g91_1() {
        let mut s = Status::default();
        s.active_gcodes = Default::default();
        s.active_gcodes[6] = 910;
        s.active_gcodes[12] = 911;
        assert_eq!(distance_mode(Some(&s)), Some("G91"));
        s.active_gcodes[6] = 900;
        assert_eq!(distance_mode(Some(&s)), Some("G90"));
        s.active_gcodes[6] = -1;
        assert_eq!(distance_mode(Some(&s)), None);
        assert_eq!(distance_mode(None), None);
    }

    #[test]
    fn values_keep_their_slot() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let lists = vec![v(&["3023", "3019", "3018"]), v(&["3019", "3018"]), v(&["3024", "3023", "3019"]), v(&["3019", "3018", "L0:stock"])];
        assert_eq!(super::slots(&lists, &lists[0]), [0, 1, 2]);
        assert_eq!(super::slots(&lists, &lists[1]), [1, 2]);
        assert_eq!(super::slots(&lists, &lists[2]), [2, 0, 1]);
        assert_eq!(super::slots(&lists, &lists[3]), [1, 2, 0]);
    }

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

    fn probe() -> Probe {
        Probe {
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
            defaults: BTreeMap::new(),
            results: Vec::new(),
            result_text: String::new(),
            flow: None,
            token: 7,
            run: None,
            status: None,
            state: Settings::memory(),
            restore_pending: false,
            restore_after: None,
            quiet_since: None,
            job_armed: false,
            probe_tool: 99,
            sources: BTreeMap::new(),
            toml: None,
            machine: machine::spawn(Box::new(machine::Sim::new()), Duration::from_secs(3600), |_| {}),
            io: None,
            ui: slint::Weak::default(),
            rotation: Box::new(|_| 0.0),
            me: std::rc::Weak::new(),
        }
    }

    /// The macro call for the current selection (the stock/two-face token
    /// is the one after 7).
    fn call_now(p: &mut Probe) -> String {
        let t = p.selected();
        p.token = 7;
        p.call(&t).unwrap().0
    }

    fn pick(p: &mut Probe, mode: &'static str, shape: &'static str) {
        p.mode = mode;
        p.shape = shape;
        p.settle();
    }

    #[test]
    fn job_calls_match_the_screen_calls() {
        let mut p = probe();
        for (mode, shapes) in SHAPES {
            for (shape, _) in shapes.iter() {
                pick(&mut p, mode, shape);
                for ridge in [false, true] {
                    p.ridge = ridge;
                    for t in p.targets() {
                        if let (Some(_), Some(plain)) = (p.job_routine(&t), p.plain_call(&t)) {
                            assert_eq!(p.call(&t).unwrap().0, plain, "{shape} {}", t.id);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn block_center_choices_pick_the_same_routines() {
        let mut p = probe();
        p.tgt = "ctr";
        p.settle();
        assert_eq!(call_now(&mut p), "o<probe_auto_stock_center> call [2] [150.000] [8]");
        p.option("start-L");
        assert_eq!(call_now(&mut p), "o<probe_auto_stock_center> call [0] [150.000] [8]");
        p.option("axis-X");
        assert_eq!((p.both, p.start, p.axes), (false, 'L', "X"));
        assert_eq!(call_now(&mut p), "o<probe_two_face> call [0] [1] [1] [8]");
        p.option("start-R");
        assert_eq!(call_now(&mut p), "o<probe_two_face> call [0] [-1] [1] [8]");
        // Front is disabled with X only: the tap changes nothing.
        p.option("start-F");
        assert_eq!(p.start, 'R');
        p.option("axis-Y");
        assert_eq!((p.start, p.axes), ('F', "Y"));
        assert_eq!(call_now(&mut p), "o<probe_two_face> call [1] [1] [1] [8]");
        p.option("start-B");
        assert_eq!(call_now(&mut p), "o<probe_two_face> call [1] [-1] [1] [8]");
        p.option("both");
        assert_eq!(call_now(&mut p), "o<probe_auto_stock_center> call [3] [150.000] [8]");
    }

    #[test]
    fn option_rows_pick_the_same_routines() {
        let mut p = probe();
        p.tgt = "e-l";
        assert_eq!(call_now(&mut p), "o<probe_x_plus_wco> call");
        p.option("angle-on");
        assert_eq!(call_now(&mut p), "o<probe_top_left_edge_angle> call");
        p.option("angle-off");
        assert_eq!(call_now(&mut p), "o<probe_x_plus_wco> call");
        pick(&mut p, "find", "slot");
        assert_eq!(call_now(&mut p), "o<probe_ridge_x> call");
        p.option("valley");
        p.option("Y");
        assert_eq!(call_now(&mut p), "o<probe_valley_y_center_start> call");
        pick(&mut p, "find", "cyl");
        assert_eq!(call_now(&mut p), "o<probe_a_setup_cylinder_center> call [30.000] [1] [0]");
        p.option("tgt-flat");
        assert_eq!(call_now(&mut p), "o<probe_a_rotary> call [1]");
        p.option("tgt-ctr");
        assert_eq!(call_now(&mut p), "o<probe_a_setup_cylinder_center> call [30.000] [1] [0]");
    }

    /// Every target of every shape: at most three values on the screen (the
    /// three fixed slots), every routine still reachable, and the option rows
    /// never offer more buttons than fit.
    #[test]
    fn every_target_fits_and_every_routine_is_reachable() {
        let mut p = probe();
        let mut macros = std::collections::BTreeSet::new();
        for (mode, shapes) in SHAPES {
            for (shape, _) in *shapes {
                pick(&mut p, mode, shape);
                let combos: [&[&str]; 8] = [&[], &["angle-on"], &["axis-X"], &["axis-Y"], &["valley"], &["Y"], &["valley", "Y"], &["tgt-flat"]];
                for opts in combos {
                    for o in opts {
                        p.option(o);
                    }
                    for t in p.targets() {
                        p.tgt = t.id;
                        let sel = p.selected();
                        let shown = p.key_params(sel.kind).len() + locals(sel.kind).len();
                        assert!(shown <= 3, "{mode}/{shape}/{}: {shown} values", sel.id);
                        let (_, o1, _, o2) = p.options(&sel);
                        assert!(o1.len() <= 4 && o2.len() <= 4);
                        macros.insert(call_now(&mut p).split_whitespace().next().unwrap().to_string());
                    }
                    p.angle = false;
                    p.both = true;
                    p.ridge = true;
                    p.axes = "XY";
                    p.settle();
                }
            }
        }
        let want: Vec<String> = MACROS.iter().map(|m| format!("o<{m}>")).collect();
        assert_eq!(macros.into_iter().collect::<Vec<_>>(), want);
    }

    #[test]
    fn values_format() {
        assert_eq!(fmt_value(3019, 2.0), "2");
        assert_eq!(fmt_value(3032, 0.059), "0.059");
        assert_eq!(fmt_value(3030, 1.0), "POSITION ONLY");
    }

    #[test]
    fn values_in_inches() {
        use crate::machine_info::Units::{Inch, Mm};
        // Four places, the units the macros use, steps in inches.
        assert_eq!(fmt_in(3032, 0.00234, Inch), "0.0023");
        assert_eq!(fmt_in(3032, 0.00234, Mm), "0.002");
        assert_eq!(fmt_in(3019, 0.2, Inch), "0.2");
        assert_eq!(unit_of(param(3015).unwrap().unit, Inch), "in/min");
        assert_eq!(unit_of(param(3019).unwrap().unit, Inch), "in");
        assert_eq!(unit_of(param(3015).unwrap().unit, Mm), "mm/min");
        assert_eq!(unit_of(param(3030).unwrap().unit, Inch), "");
        assert_eq!(step_of(3019, Inch), 0.02);
        assert_eq!(step_of(3019, Mm), 0.5);
        assert_eq!(step_of(3016, Inch), 0.5);
        assert_eq!(step_of(3018, Mm), 1.0);
        assert_eq!(step_of(3036, Inch), 1.0);
    }
}
