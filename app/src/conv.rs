//! CONV screen (milestone 6): conversational facing, drilling, helical bore
//! and thread milling.
//!
//! The inputs are grouped into four sections (SHAPE, DEPTH, TOOL, SETUP);
//! one is open at a time beside the sketch, and a recap lists all four in
//! a line each, so the whole operation reads at a glance.
//!
//! The screen only collects numbers. The G-code comes from the mill_ops
//! generators in the jobcore crate (through the job session), so ADD TO JOB and LOAD AS
//! PROGRAM cut exactly what the Probe Basic pages did. Every change is checked
//! by the generator at once and its summary or errors shown.

use crate::job::Job;
use crate::manual::KeypadTarget;
use crate::settings::Settings;
use crate::{Actions, ConvField, Live, MainWindow, ProbeItem};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

/// Screen id, label, generator type.
const TYPES: [(&str, &str, &str); 6] = [
    ("face", "FACING", "face"),
    ("xy", "XY COORD", "drill"),
    ("grid", "PATTERN", "drill"),
    ("circle", "HOLE CIRCLE", "drill"),
    ("bore", "HELICAL BORE", "helical_bore"),
    ("thread", "THREAD MILL", "thread_mill"),
];

const WCS: [&str; 9] = ["G54", "G55", "G56", "G57", "G58", "G59", "G59.1", "G59.2", "G59.3"];
const CYCLES: [&str; 7] = ["DRILL", "PECK", "BREAK", "DWELL", "TAP", "RIGID TAP", "MANUAL"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A length in program units.
    Len,
    Feed,
    Rpm,
    Count,
    Deg,
    Choice(&'static [&'static str]),
    /// Thread presets come from the helper.
    Preset,
}

struct F {
    key: &'static str,
    label: &'static str,
    kind: Kind,
}

const fn f(key: &'static str, label: &'static str, kind: Kind) -> F {
    F { key, label, kind }
}

const SETUP: [F; 12] = [
    f("tool_number", "TOOL", Kind::Count),
    f("spindle_rpm", "RPM", Kind::Rpm),
    f("spindle_dir", "SPINDLE", Kind::Choice(&["CW", "CCW"])),
    f("wcs", "WORK OFFSET", Kind::Choice(&WCS)),
    f("units", "UNITS", Kind::Choice(&["MM", "IN"])),
    f("coolant", "COOLANT", Kind::Choice(&["OFF", "MIST", "FLOOD"])),
    f("xy_feed", "XY FEED", Kind::Feed),
    f("z_feed", "Z FEED", Kind::Feed),
    f("z_clear", "CLEARANCE Z", Kind::Len),
    f("retract", "RETRACT ABOVE START", Kind::Len),
    f("z_start", "Z START", Kind::Len),
    f("z_end", "Z END", Kind::Len),
];

const FACE: [F; 6] = [
    f("x_start", "X START", Kind::Len),
    f("x_end", "X END", Kind::Len),
    f("y_start", "Y START", Kind::Len),
    f("y_end", "Y END", Kind::Len),
    f("step_over", "STEP OVER", Kind::Len),
    f("step_down", "STEP DOWN", Kind::Len),
];
const DRILL: [F; 3] = [
    f("cycle", "CYCLE", Kind::Choice(&CYCLES)),
    f("cycle_param", "", Kind::Len),
    f("retract_mode", "BETWEEN HOLES", Kind::Choice(&["G98", "G99"])),
];
const XY: [F; 2] = [f("hole_x", "HOLE X", Kind::Len), f("hole_y", "HOLE Y", Kind::Len)];
const GRID: [F; 6] = [
    f("x0", "FIRST HOLE X", Kind::Len),
    f("y0", "FIRST HOLE Y", Kind::Len),
    f("cols", "COLUMNS", Kind::Count),
    f("rows", "ROWS", Kind::Count),
    f("dx", "X PITCH", Kind::Len),
    f("dy", "Y PITCH", Kind::Len),
];
const CIRCLE: [F; 5] = [
    f("cx", "CENTER X", Kind::Len),
    f("cy", "CENTER Y", Kind::Len),
    f("circle_d", "CIRCLE DIA", Kind::Len),
    f("count", "HOLES", Kind::Count),
    f("start_angle", "START ANGLE", Kind::Deg),
];
const BORE: [F; 4] = [
    f("cx", "CENTER X", Kind::Len),
    f("cy", "CENTER Y", Kind::Len),
    f("diameter", "HOLE DIA", Kind::Len),
    f("ramp_angle", "RAMP ANGLE", Kind::Deg),
];
const THREAD: [F; 7] = [
    f("preset", "THREAD", Kind::Preset),
    f("cx", "CENTER X", Kind::Len),
    f("cy", "CENTER Y", Kind::Len),
    f("diameter", "MAJOR DIA", Kind::Len),
    f("pitch", "PITCH", Kind::Len),
    f("radial_passes", "RADIAL PASSES", Kind::Count),
    f("hand", "HAND", Kind::Choice(&["RH", "LH"])),
];

fn op_fields(ty: &str) -> Vec<&'static F> {
    match ty {
        "face" => FACE.iter().collect(),
        "xy" => DRILL.iter().chain(XY.iter()).collect(),
        "grid" => DRILL.iter().chain(GRID.iter()).collect(),
        "circle" => DRILL.iter().chain(CIRCLE.iter()).collect(),
        "bore" => BORE.iter().collect(),
        _ => THREAD.iter().collect(),
    }
}

/// Section id, tab label.
const SECTIONS: [(&str, &str); 4] = [("shape", "SHAPE"), ("depth", "DEPTH"), ("tool", "TOOL"), ("setup", "SETUP")];

/// The fields a section shows, in order. Every field of the operation is
/// in exactly one section.
fn section_keys(ty: &str, section: &str) -> &'static [&'static str] {
    match (section, ty) {
        ("shape", "face") => &["x_start", "x_end", "y_start", "y_end"],
        ("shape", "xy") => &["hole_x", "hole_y"],
        ("shape", "grid") => &["x0", "y0", "cols", "rows", "dx", "dy"],
        ("shape", "circle") => &["cx", "cy", "circle_d", "count", "start_angle"],
        ("shape", "bore") => &["cx", "cy", "diameter"],
        ("shape", _) => &["preset", "hand", "cx", "cy", "diameter", "pitch"],
        // Clearance and retract are Z levels, so they sit with the depths.
        ("depth", "face") => &["z_start", "z_end", "z_clear", "retract", "step_down"],
        ("depth", "xy" | "grid" | "circle") => &["z_start", "z_end", "z_clear", "retract", "cycle", "cycle_param", "retract_mode"],
        ("depth", "bore") => &["z_start", "z_end", "z_clear", "retract", "ramp_angle"],
        ("depth", _) => &["z_start", "z_end", "z_clear", "retract"],
        ("tool", "face") => &["tool_number", "spindle_rpm", "xy_feed", "z_feed", "step_over"],
        ("tool", "thread") => &["tool_number", "spindle_rpm", "xy_feed", "z_feed", "radial_passes"],
        ("tool", _) => &["tool_number", "spindle_rpm", "xy_feed", "z_feed"],
        _ => &["wcs", "units", "coolant", "spindle_dir"],
    }
}

fn gen_type(ty: &str) -> &'static str {
    TYPES.iter().find(|t| t.0 == ty).map_or("face", |t| t.2)
}

fn orbit(ty: &str) -> bool {
    matches!(ty, "bore" | "thread")
}

/// Starting values, metric, from the mill's usual tools (mockup).
fn default(ty: &str, key: &str) -> &'static str {
    let drill = matches!(ty, "xy" | "grid" | "circle");
    match key {
        "wcs" => "G54",
        "units" => "MM",
        "coolant" => "OFF",
        "spindle_dir" => "CW",
        "z_clear" => "10",
        "retract" => if ty == "thread" { "1" } else { "2" },
        "z_start" => "0",
        "tool_number" => match ty { "face" => "14", "bore" => "23", "thread" => "110", _ => "7" },
        "spindle_rpm" => match ty { "face" => "3200", "bore" | "thread" => "4250", _ => "1800" },
        "xy_feed" => match ty { "face" => "900", "thread" => "300", _ => "500" },
        "z_feed" => if drill { "120" } else { "300" },
        "z_end" => match ty { "face" => "-0.5", "bore" => "-10", _ => "-8" },
        "x_start" | "y_start" => "0",
        "x_end" => "100",
        "y_end" => "-60",
        "step_over" => "35",
        "step_down" => "0.5",
        "cycle" => "PECK",
        "cycle_param" => "2",
        "retract_mode" => "G98",
        "hole_x" | "hole_y" => "0",
        "x0" => "10",
        "y0" => "-10",
        "cols" | "rows" => "2",
        "dx" => "80",
        "dy" => "-40",
        "cx" => if ty == "circle" { "50" } else { "0" },
        "cy" => if ty == "circle" { "-30" } else { "0" },
        "circle_d" => "30",
        "count" => "6",
        "start_angle" => "0",
        "diameter" => if ty == "thread" { "6" } else { "12" },
        "ramp_angle" => "2",
        "preset" => "M6 x 1",
        "pitch" => "1",
        "drill" => "5",
        "radial_passes" => "2",
        "hand" => "RH",
        _ => "0",
    }
}

fn fmt(v: f64) -> String {
    let t = format!("{v:.4}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() || t == "-0" { "0".into() } else { t.into() }
}

pub struct Conv {
    ty: String,
    /// The open section.
    section: String,
    settings: Settings,
    hole_sel: Option<usize>,
    /// The choice field the picker is open for.
    picking: Option<&'static str>,
    presets: Vec<String>,
    summary: String,
    error: bool,
    /// Facing: the generated feed moves at the final depth (XY), so the
    /// sketch shows the real passes, tool clearance and lead-in arcs.
    face_moves: Vec<((f64, f64), (f64, f64))>,
    job: Rc<RefCell<Job>>,
    ui: slint::Weak<MainWindow>,
}

impl Conv {
    fn get(&self, key: &str) -> String {
        self.settings.get(&format!("conv.{}.{key}", self.ty)).map(str::to_string).unwrap_or_else(|| default(&self.ty, key).to_string())
    }

    fn num(&self, key: &str) -> f64 {
        self.get(key).parse().unwrap_or(0.0)
    }

    fn set(&mut self, key: &str, v: impl ToString) {
        let k = format!("conv.{}.{key}", self.ty);
        self.settings.set(&k, v);
    }

    fn holes(&self) -> Vec<(f64, f64)> {
        match self.ty.as_str() {
            "xy" => {
                let saved = self.settings.list(&format!("conv.xy.hole."));
                if saved.is_empty() && self.settings.get("conv.xy.holes-set").is_none() {
                    return vec![(10.0, -10.0), (90.0, -10.0), (90.0, -50.0), (10.0, -50.0)];
                }
                saved.iter().filter_map(|v| v.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))).collect()
            }
            "grid" => {
                let (x0, y0, dx, dy) = (self.num("x0"), self.num("y0"), self.num("dx"), self.num("dy"));
                let (cols, rows) = (self.num("cols").max(0.0) as usize, self.num("rows").max(0.0) as usize);
                (0..rows.min(50)).flat_map(|r| (0..cols.min(50)).map(move |c| (x0 + c as f64 * dx, y0 + r as f64 * dy))).collect()
            }
            "circle" => {
                let n = self.num("count").max(0.0) as usize;
                let (cx, cy, r, a0) = (self.num("cx"), self.num("cy"), self.num("circle_d") / 2.0, self.num("start_angle"));
                (0..n.min(360))
                    .map(|i| {
                        let a = (a0 + i as f64 * 360.0 / n as f64) * PI / 180.0;
                        (cx + a.cos() * r, cy + a.sin() * r)
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn set_holes(&mut self, holes: &[(f64, f64)]) {
        self.settings.set("conv.xy.holes-set", "1");
        self.settings.set_list("conv.xy.hole.", holes.iter().map(|(x, y)| format!("{},{}", fmt(*x), fmt(*y))));
    }

    fn label(&self, f: &F) -> String {
        match f.key {
            "cycle_param" => match self.get("cycle").as_str() {
                "PECK" => "PECK DEPTH",
                "BREAK" => "CHIP BREAK DEPTH",
                "DWELL" => "DWELL SECONDS",
                "TAP" | "RIGID TAP" => "THREAD PITCH",
                _ => "NO PARAMETER",
            }
            .into(),
            "xy_feed" if orbit(&self.ty) => "FEED".into(),
            _ => f.label.into(),
        }
    }

    fn visible(&self, f: &F) -> bool {
        match f.key {
            "z_feed" => !orbit(&self.ty),
            _ => true,
        }
    }

    fn unit(&self, f: &F) -> String {
        let u = self.get("units").to_lowercase();
        match f.kind {
            Kind::Len if f.key == "cycle_param" && self.get("cycle") == "DWELL" => "s".into(),
            Kind::Len => u,
            Kind::Feed => format!("{u}/min"),
            Kind::Rpm => "rpm".into(),
            Kind::Deg => "°".into(),
            _ => String::new(),
        }
    }

    fn unused(&self, key: &str) -> bool {
        key == "cycle_param" && matches!(self.get("cycle").as_str(), "DRILL" | "MANUAL")
    }

    fn row(&self, f: &F) -> ConvField {
        if self.unused(f.key) {
            return ConvField { key: f.key.into(), label: self.label(f).into(), value: "—".into(), unit: "".into(), choice: false };
        }
        // Degrees read as part of the number.
        let (value, unit) = if f.kind == Kind::Deg { (format!("{}°", self.get(f.key)), String::new()) } else { (self.get(f.key), self.unit(f)) };
        ConvField { key: f.key.into(), label: self.label(f).into(), value: value.into(), unit: unit.into(), choice: matches!(f.kind, Kind::Choice(_) | Kind::Preset) }
    }

    fn find(&self, key: &str) -> Option<&'static F> {
        SETUP.iter().chain(op_fields(&self.ty)).find(|f| f.key == key)
    }

    /// One line saying what a section is set to.
    fn recap(&self, section: &str) -> String {
        let g = |k: &str| self.get(k);
        let u = g("units").to_lowercase();
        match (section, self.ty.as_str()) {
            ("shape", "face") => format!("X {} to {}  ·  Y {} to {}", g("x_start"), g("x_end"), g("y_start"), g("y_end")),
            ("shape", "xy") => {
                let n = self.holes().len();
                format!("{n} hole{}", if n == 1 { "" } else { "s" })
            }
            ("shape", "grid") => format!("{} x {} holes from X {} Y {}  ·  pitch {} / {}", g("cols"), g("rows"), g("x0"), g("y0"), g("dx"), g("dy")),
            ("shape", "circle") => format!("{} holes on Ø{} at X {} Y {}", g("count"), g("circle_d"), g("cx"), g("cy")),
            ("shape", "bore") => format!("Ø{} at X {} Y {}", g("diameter"), g("cx"), g("cy")),
            ("shape", _) => {
                let what = if g("preset") == "CUSTOM" { format!("Ø{} x {}", g("diameter"), g("pitch")) } else { g("preset") };
                format!("{what} {} at X {} Y {}", g("hand"), g("cx"), g("cy"))
            }
            ("depth", ty) => {
                let z = format!("Z {} to {}  ·  clear Z {}", g("z_start"), g("z_end"), g("z_clear"));
                match ty {
                    "face" => format!("{z}  ·  step down {}", g("step_down")),
                    "bore" => format!("{z}  ·  ramp {}°", g("ramp_angle")),
                    "thread" => z,
                    _ if self.unused("cycle_param") => format!("{z}  ·  {}", g("cycle")),
                    _ => format!("{z}  ·  {} {}", g("cycle"), g("cycle_param")),
                }
            }
            ("tool", ty) => {
                // Drilling feeds in Z; everything else is described by its XY feed.
                let feed = if matches!(ty, "xy" | "grid" | "circle") { g("z_feed") } else { g("xy_feed") };
                let t = format!("T{}  ·  {} rpm  ·  {feed} {u}/min", g("tool_number"), g("spindle_rpm"));
                match ty {
                    "face" => format!("{t}  ·  step over {}", g("step_over")),
                    "thread" => format!("{t}  ·  {} radial passes", g("radial_passes")),
                    _ => t,
                }
            }
            _ => format!("{}  ·  {}  ·  coolant {}  ·  spindle {}", g("wcs"), g("units"), g("coolant").to_lowercase(), g("spindle_dir")),
        }
    }

    /// The generator's inputs, as the job session expects them.
    fn request(&self, action: &str) -> Value {
        let mut attrs = serde_json::Map::new();
        for f in SETUP.iter() {
            let v = self.get(f.key);
            attrs.insert(f.key.into(), match f.kind {
                Kind::Choice(_) => json!(v),
                Kind::Count => json!(self.num(f.key) as i64),
                _ => json!(self.num(f.key)),
            });
        }
        let ty = gen_type(&self.ty);
        match ty {
            "face" => {
                for f in FACE.iter() {
                    attrs.insert(f.key.into(), json!(self.num(f.key)));
                }
            }
            "drill" => {
                attrs.insert("holes".into(), json!(self.holes().iter().map(|(x, y)| [x, y]).collect::<Vec<_>>()));
                attrs.insert("retract_mode".into(), json!(self.get("retract_mode")));
            }
            _ => {
                attrs.insert("center".into(), json!([self.num("cx"), self.num("cy")]));
                attrs.insert("diameter".into(), json!(self.num("diameter")));
                if ty == "helical_bore" {
                    attrs.insert("ramp_angle".into(), json!(self.num("ramp_angle")));
                } else {
                    attrs.insert("pitch".into(), json!(self.num("pitch")));
                    attrs.insert("radial_passes".into(), json!(self.num("radial_passes") as i64));
                    attrs.insert("hand".into(), json!(self.get("hand")));
                    if self.get("preset") != "CUSTOM" {
                        attrs.insert("pre_drill".into(), json!(self.num("drill")));
                    }
                }
            }
        }
        let name = format!("conv-{}", self.ty);
        json!({"cmd": "conv", "type": ty, "action": action, "attrs": attrs, "name": name,
               "cycle": self.get("cycle"), "cycle_param": self.num("cycle_param")})
    }

    /// Ask the generator. Returns the reply for add/post.
    fn run(&mut self, action: &str) -> Option<Value> {
        let req = self.request(action);
        let r = self.job.borrow_mut().call(req);
        match r {
            Ok(r) => {
                self.face_moves = r["gcode"].as_str().map_or_else(Vec::new, |g| {
                    let tp = preview::mini::parse(g, 100.0);
                    let zmin = tp.segs.iter().filter(|s| s.kind == preview::Kind::Feed).map(|s| s.to[2]).fold(f64::MAX, f64::min);
                    tp.segs
                        .iter()
                        .filter(|s| s.kind == preview::Kind::Feed && (s.from[2] - zmin).abs() < 1e-6 && (s.to[2] - zmin).abs() < 1e-6)
                        .map(|s| ((s.from[0], s.from[1]), (s.to[0], s.to[1])))
                        .collect()
                });
                let errors: Vec<String> = r["errors"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect()).unwrap_or_default();
                self.error = !errors.is_empty();
                self.summary = if self.error {
                    errors.join("\n")
                } else {
                    r["summary"].as_str().unwrap_or("").to_string()
                };
                // ADD / LOAD results go to the message line; the summary
                // keeps describing the operation.
                if let (false, Some(m), Some(ui)) = (self.error, r["message"].as_str(), self.ui.upgrade()) {
                    ui.global::<Live>().set_message(m.into());
                    ui.global::<Live>().set_message_error(false);
                }
                (!self.error).then_some(r)
            }
            Err(e) => {
                self.error = true;
                self.summary = e;
                None
            }
        }
    }

    fn tool_dia(&self) -> f64 {
        let t = self.num("tool_number") as i32;
        let path = self.job.borrow().tool_table();
        let tools = std::fs::read_to_string(path).map(|s| crate::tools::parse(&s)).unwrap_or_default();
        tools.iter().find(|x| x.t == t).map_or(0.0, |x| x.d)
    }

    fn tool_label(&self) -> String {
        let t = self.num("tool_number") as i32;
        let path = self.job.borrow().tool_table();
        let tools = std::fs::read_to_string(path).map(|s| crate::tools::parse(&s)).unwrap_or_default();
        match tools.iter().find(|x| x.t == t) {
            Some(x) => format!("T{t}  ·  Ø{}  ·  {}", fmt(x.d), x.comment.to_uppercase()),
            None => format!("T{t} IS NOT IN THE TOOL TABLE"),
        }
    }

    /// Outline and toolpath sketch in a 400x300 box.
    /// Outline, toolpath, and the cutter's width in sketch units (facing
    /// draws the path that wide, so the sketch shows what gets cut).
    fn picture(&self) -> (String, String, f64) {
        let mut swept = 0.0;
        let mut outline = String::new();
        let mut path = String::new();
        // Fit points (part coordinates, Y up) into the box.
        let fit = |pts: &[(f64, f64)]| -> Box<dyn Fn(f64, f64) -> (f64, f64)> {
            let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
            for &(x, y) in pts {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
            let w = (x1 - x0).max(1e-6);
            let h = (y1 - y0).max(1e-6);
            let s = (340.0 / w).min(240.0 / h);
            let (ox, oy) = (200.0 - (x0 + x1) / 2.0 * s, 150.0 + (y0 + y1) / 2.0 * s);
            Box::new(move |x, y| (ox + x * s, oy - y * s))
        };
        let circle = |out: &mut String, (cx, cy): (f64, f64), r: f64| {
            out.push_str(&format!("M{:.1} {:.1}a{r:.1} {r:.1} 0 1 0 {:.1} 0a{r:.1} {r:.1} 0 1 0 {:.1} 0", cx - r, cy, 2.0 * r, -2.0 * r));
        };
        match self.ty.as_str() {
            "face" => {
                let (xs, xe, ys, ye) = (self.num("x_start"), self.num("x_end"), self.num("y_start"), self.num("y_end"));
                let mut pts = vec![(xs, ys), (xe, ye), (0.0, 0.0)];
                // The swept band reaches a cutter radius past each pass.
                let r = self.tool_dia() / 2.0;
                pts.extend(self.face_moves.iter().flat_map(|(a, b)| [*a, *b]).flat_map(|(x, y)| [(x - r, y - r), (x + r, y + r)]));
                let m = fit(&pts);
                swept = (m(self.tool_dia(), 0.0).0 - m(0.0, 0.0).0).abs();
                let (a, b) = (m(xs, ys), m(xe, ye));
                outline.push_str(&format!("M{:.1} {:.1}H{:.1}V{:.1}H{:.1}Z", a.0, a.1, b.0, b.1, a.0));
                let o = m(0.0, 0.0);
                outline.push_str(&format!("M{:.1} {:.1}h16M{:.1} {:.1}v16", o.0 - 8.0, o.1, o.0, o.1 - 8.0));
                // The generator's own passes; consecutive moves join up.
                let mut last: Option<(f64, f64)> = None;
                for (p, q) in &self.face_moves {
                    let (p, q) = (m(p.0, p.1), m(q.0, q.1));
                    if last.is_none_or(|l| (l.0 - p.0).abs() > 0.05 || (l.1 - p.1).abs() > 0.05) {
                        path.push_str(&format!("M{:.1} {:.1}", p.0, p.1));
                    }
                    path.push_str(&format!("L{:.1} {:.1}", q.0, q.1));
                    last = Some(q);
                }
            }
            "xy" | "grid" | "circle" => {
                let holes = self.holes();
                let mut pts = holes.clone();
                pts.push((0.0, 0.0));
                let m = fit(&pts);
                if self.ty == "circle" {
                    let c = m(self.num("cx"), self.num("cy"));
                    let r = (m(self.num("circle_d") / 2.0, 0.0).0 - m(0.0, 0.0).0).abs();
                    circle(&mut outline, c, r);
                }
                let o = m(0.0, 0.0);
                outline.push_str(&format!("M{:.1} {:.1}h16M{:.1} {:.1}v16", o.0 - 8.0, o.1, o.0, o.1 - 8.0));
                for (i, &(x, y)) in holes.iter().enumerate() {
                    circle(&mut path, m(x, y), if self.ty == "xy" && self.hole_sel == Some(i) { 13.0 } else { 9.0 });
                }
            }
            _ => {
                let d = self.num("diameter").max(0.1);
                let s = 220.0 / d;
                let c = (200.0, 150.0);
                circle(&mut outline, c, d / 2.0 * s);
                outline.push_str("M200 25V275M75 150H325");
                let tool = self.job.borrow().tool_diameter(self.num("tool_number") as i32);
                if self.ty == "bore" {
                    let fin = ((d - tool) / 2.0).max(0.0);
                    let passes = if tool > 0.0 { (fin / (tool / 2.0) - 1e-9).ceil().clamp(1.0, 20.0) as usize } else { 1 };
                    for k in 1..=passes {
                        circle(&mut path, c, fin * k as f64 / passes as f64 * s);
                    }
                } else {
                    let minor = d - 2.0 * 0.5413 * self.num("pitch");
                    circle(&mut outline, c, minor / 2.0 * s);
                    circle(&mut path, c, ((d - tool) / 2.0).max(0.0) * s);
                }
            }
        }
        (outline, path, swept)
    }

    fn show(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        let types: Vec<ProbeItem> = TYPES.iter().map(|(id, label, _)| ProbeItem { id: (*id).into(), label: (*label).into(), on: *id == self.ty }).collect();
        live.set_conv_types(ModelRc::new(VecModel::from(types)));
        live.set_conv_title(TYPES.iter().find(|t| t.0 == self.ty).map_or("", |t| t.1).into());
        live.set_conv_tool(self.tool_label().into());
        let sections: Vec<ProbeItem> = SECTIONS.iter().map(|(id, label)| ProbeItem { id: (*id).into(), label: (*label).into(), on: *id == self.section }).collect();
        live.set_conv_sections(ModelRc::new(VecModel::from(sections)));
        live.set_conv_section(self.section.as_str().into());
        let fields: Vec<ConvField> = section_keys(&self.ty, &self.section).iter().filter_map(|k| self.find(k)).filter(|f| self.visible(f)).map(|f| self.row(f)).collect();
        live.set_conv_fields(ModelRc::new(VecModel::from(fields)));
        let recap: Vec<ConvField> = SECTIONS
            .iter()
            .map(|(id, label)| ConvField { key: (*id).into(), label: (*label).into(), value: self.recap(id).into(), unit: "".into(), choice: false })
            .collect();
        live.set_conv_recap(ModelRc::new(VecModel::from(recap)));
        let levels: Vec<ConvField> = [("z_clear", "CLEARANCE Z"), ("retract", "RETRACT ABOVE START"), ("z_start", "Z START"), ("z_end", "Z END")]
            .iter()
            .map(|(k, label)| ConvField { key: (*k).into(), label: (*label).into(), value: self.get(k).into(), unit: self.get("units").to_lowercase().into(), choice: false })
            .collect();
        live.set_conv_levels(ModelRc::new(VecModel::from(levels)));
        live.set_conv_show_holes(self.ty == "xy" && self.section == "shape");
        let holes: Vec<SharedString> = if self.ty == "xy" {
            self.holes().iter().enumerate().map(|(i, (x, y))| format!("{}   X {}   Y {}", i + 1, fmt(*x), fmt(*y)).into()).collect()
        } else {
            Vec::new()
        };
        live.set_conv_holes(ModelRc::new(VecModel::from(holes)));
        live.set_conv_hole_sel(self.hole_sel.map_or(-1, |i| i as i32));
        let (outline, path, swept) = self.picture();
        live.set_conv_swept(swept as f32);
        live.set_conv_outline(outline.into());
        live.set_conv_path(path.into());
        live.set_conv_summary(self.summary.clone().into());
        live.set_conv_error(self.error);
    }

    fn update(&mut self) {
        self.run("check");
        self.refresh();
    }

    fn refresh(&self) {
        if let Some(ui) = self.ui.upgrade() {
            self.show(&ui);
        }
    }
}

/// Keypad values "cv-<field>".
struct Keys(Rc<RefCell<Conv>>);

impl KeypadTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        let key = what.strip_prefix("cv-")?;
        let c = self.0.borrow();
        let f = c.find(key)?;
        let u = c.unit(f);
        Some(if u.is_empty() { c.label(f) } else { format!("{} {}", c.label(f), u.to_uppercase()) })
    }
    fn current(&self, what: &str) -> Option<f64> {
        let key = what.strip_prefix("cv-")?;
        let c = self.0.borrow();
        c.find(key)?;
        Some(c.num(key))
    }
    fn apply(&self, what: &str, v: f64) -> Option<Result<(), String>> {
        let key = what.strip_prefix("cv-")?;
        let mut c = self.0.borrow_mut();
        let f = c.find(key)?;
        let v = if f.kind == Kind::Count { v.round() } else { v };
        if matches!(f.kind, Kind::Count | Kind::Rpm | Kind::Feed) && v < 0.0 {
            return Some(Err("Must not be negative".into()));
        }
        c.set(key, fmt(v));
        if c.ty == "thread" && matches!(key, "diameter" | "pitch") {
            c.set("preset", "CUSTOM");
        }
        c.update();
        Some(Ok(()))
    }
}

pub fn wire(ui: &MainWindow, job: Rc<RefCell<Job>>, settings: Settings) -> Rc<RefCell<Conv>> {
    let presets = job
        .borrow_mut()
        .call(json!({"cmd": "presets"}))
        .ok()
        .and_then(|r| r["presets"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()))
        .unwrap_or_default();
    let ty = settings.get("conv.type").unwrap_or("face").to_string();
    let c = Rc::new(RefCell::new(Conv { ty, section: "shape".into(), settings, hole_sel: None, picking: None, presets, summary: String::new(), error: false, face_moves: Vec::new(), job, ui: ui.as_weak() }));
    crate::manual::add_keypad_target(Rc::new(Keys(c.clone())));
    c.borrow_mut().update();

    let actions = ui.global::<Actions>();
    let t = c.clone();
    actions.on_conv_type(move |id| {
        let mut c = t.borrow_mut();
        if TYPES.iter().any(|x| x.0 == id.as_str()) {
            c.ty = id.to_string();
            let ty = c.ty.clone();
            c.settings.set("conv.type", ty);
            c.hole_sel = None;
            c.update();
        }
    });
    let t = c.clone();
    actions.on_conv_section(move |id| {
        let mut c = t.borrow_mut();
        if SECTIONS.iter().any(|x| x.0 == id.as_str()) {
            c.section = id.to_string();
            c.refresh();
        }
    });
    let (t, weak) = (c.clone(), ui.as_weak());
    actions.on_conv_field(move |key| {
        let Some(ui) = weak.upgrade() else { return };
        let mut c = t.borrow_mut();
        let Some(f) = c.find(&key) else { return };
        if c.unused(f.key) {
            return;
        }
        match f.kind {
            Kind::Choice(opts) => {
                c.picking = Some(f.key);
                let cur = c.get(f.key);
                crate::picker::open(&ui, "conv", &c.label(f), opts.iter().map(|s| s.to_string()).collect(), opts.iter().position(|o| *o == cur));
            }
            Kind::Preset => {
                c.picking = Some(f.key);
                let mut items = c.presets.clone();
                items.push("CUSTOM".into());
                let cur = c.get("preset");
                let at = items.iter().position(|o| *o == cur);
                crate::picker::open(&ui, "conv", "THREAD", items, at);
            }
            _ => {
                drop(c);
                ui.global::<Actions>().invoke_edit(format!("cv-{key}").into());
            }
        }
    });
    let t = c.clone();
    crate::picker::on_pick("conv", move |i| {
        let mut c = t.borrow_mut();
        let Some(key) = c.picking.take() else { return };
        let Some(f) = c.find(key) else { return };
        match f.kind {
            Kind::Choice(opts) => {
                if let Some(v) = opts.get(i) {
                    c.set(key, v);
                }
            }
            Kind::Preset => {
                let name = c.presets.get(i).cloned().unwrap_or_else(|| "CUSTOM".into());
                if name != "CUSTOM" {
                    let units = c.get("units");
                    let r = c.job.borrow_mut().call(json!({"cmd": "thread", "name": name, "units": units}));
                    if let Ok(r) = r {
                        for (k, v) in [("diameter", "major"), ("pitch", "pitch"), ("drill", "drill")] {
                            c.set(k, fmt(r[v].as_f64().unwrap_or(0.0)));
                        }
                    }
                }
                c.set("preset", name);
            }
            _ => {}
        }
        c.update();
    });
    let t = c.clone();
    actions.on_conv_hole_add(move || {
        let mut c = t.borrow_mut();
        let mut holes = c.holes();
        let h = (c.num("hole_x"), c.num("hole_y"));
        holes.push(h);
        c.set_holes(&holes);
        c.hole_sel = Some(holes.len() - 1);
        c.update();
    });
    let t = c.clone();
    actions.on_conv_hole_del(move || {
        let mut c = t.borrow_mut();
        let mut holes = c.holes();
        if let Some(i) = c.hole_sel.take().filter(|i| *i < holes.len()) {
            holes.remove(i);
            c.set_holes(&holes);
        }
        c.update();
    });
    let t = c.clone();
    actions.on_conv_hole_select(move |i| {
        let mut c = t.borrow_mut();
        let holes = c.holes();
        let i = i.max(0) as usize;
        if c.hole_sel == Some(i) {
            c.hole_sel = None;
        } else if let Some(&(x, y)) = holes.get(i) {
            c.hole_sel = Some(i);
            c.set("hole_x", fmt(x));
            c.set("hole_y", fmt(y));
        }
        c.refresh();
    });
    let t = c.clone();
    actions.on_conv_add(move || {
        let mut c = t.borrow_mut();
        let ok = c.run("add").is_some();
        let job = c.job.clone();
        c.refresh();
        drop(c);
        if ok {
            job.borrow().refresh();
        }
    });
    let (t, weak) = (c.clone(), ui.as_weak());
    actions.on_conv_load(move || {
        let mut c = t.borrow_mut();
        let ok = c.run("post").is_some();
        c.refresh();
        if let (true, Some(ui)) = (ok, weak.upgrade()) {
            ui.global::<Live>().set_screen("run".into());
        }
    });
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regrouping the screen must not drop or duplicate an input.
    #[test]
    fn every_field_is_in_exactly_one_section() {
        for (ty, _, _) in TYPES {
            let mut shown: Vec<&str> = SECTIONS.iter().flat_map(|(s, _)| section_keys(ty, s).iter().copied()).collect();
            let mut all: Vec<&str> = SETUP.iter().chain(op_fields(ty)).map(|f| f.key).collect();
            shown.sort_unstable();
            all.sort_unstable();
            assert_eq!(shown, all, "{ty}");
        }
    }
}
