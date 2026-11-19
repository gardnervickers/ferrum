//! The conversational G-code generators (probe_basic's `mill_ops.py`) and the
//! thread presets (`thread_presets.py`).

use crate::py::{self, fixed, general, Error};
use serde_json::Value;

/// The retract to the top of Z travel that every op and job starts and
/// ends with. `G53 G0 Z0` (the PM-728VT homes Z at the top and calls it 0)
/// until the screen sets the machine's own with `set_machine_top`.
pub fn machine_top() -> String {
    match MACHINE_TOP.read() {
        Ok(t) if !t.is_empty() => t.clone(),
        _ => DEFAULT_TOP.into(),
    }
}

/// Machine Z of the top of travel, as a G-code number ("0", "99.5").
pub fn set_machine_top(z: &str) {
    if let Ok(mut t) = MACHINE_TOP.write() {
        *t = format!("G53 G0 Z{z}");
    }
}

/// `machine_top` for a program in `units` ("MM" or "IN") on a machine in
/// `machine_units`: the same machine Z, converted when the program uses the
/// other units (a G53 Z is read in the units in force). With the machine's
/// units unknown, or Z 0, it is `machine_top` as is.
pub fn machine_top_in(machine_units: Option<&str>, units: &str) -> String {
    top_in(machine_top(), machine_units, units)
}

/// Machine Z of `machine_top` (0 when it cannot be read).
pub fn machine_top_z() -> f64 {
    machine_top().strip_prefix("G53 G0 Z").and_then(|z| z.parse().ok()).unwrap_or(0.0)
}

fn top_in(top: String, machine_units: Option<&str>, units: &str) -> String {
    let Some(m) = machine_units.map(py::strip_upper) else { return top };
    let u = py::strip_upper(units);
    if m == u {
        return top;
    }
    match top.strip_prefix("G53 G0 Z").and_then(|z| z.parse::<f64>().ok()) {
        Some(z) if z != 0.0 => format!("G53 G0 Z{}", crate::gcode::fmt(convert(z, &m, &u))),
        _ => top,
    }
}

const DEFAULT_TOP: &str = "G53 G0 Z0";
static MACHINE_TOP: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());
pub const DRILL_TYPES: [&str; 7] = ["DRILL", "PECK", "BREAK", "DWELL", "TAP", "RIGID TAP", "MANUAL"];
pub const THREAD_DEPTH_PER_PITCH: f64 = 0.5413;
pub const MAX_RAMP_ANGLE: f64 = 10.0;

/// Why a generator produced no G-code.
#[derive(Debug, Clone, PartialEq)]
pub enum GenError {
    /// `OperationError`: messages for the operator.
    Operation(Vec<String>),
    /// Any other exception (bad field types).
    Py(Error),
}

impl From<Error> for GenError {
    fn from(e: Error) -> Self {
        GenError::Py(e)
    }
}

type GResult<T> = std::result::Result<T, GenError>;

fn f4(x: f64) -> String {
    fixed(x, 4)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Face,
    Drill,
    HelicalBore,
    ThreadMill,
}

impl Kind {
    pub fn from_type(op_type: &str) -> Option<Kind> {
        Some(match op_type {
            "face" => Kind::Face,
            "drill" => Kind::Drill,
            "helical_bore" => Kind::HelicalBore,
            "thread_mill" => Kind::ThreadMill,
            _ => return None,
        })
    }
    pub fn class_name(self) -> &'static str {
        match self {
            Kind::Face => "MillFaceOps",
            Kind::Drill => "MillDrillOps",
            Kind::HelicalBore => "MillHelicalBoreOps",
            Kind::ThreadMill => "MillThreadMillOps",
        }
    }
    fn orbit(self) -> bool {
        matches!(self, Kind::HelicalBore | Kind::ThreadMill)
    }
}

/// One generator. Python has a class per op type; here one struct holds the
/// union of their attributes and `kind` says which exist.
#[derive(Clone, Debug)]
pub struct Generator {
    pub kind: Kind,
    pub wcs: String,
    pub coolant: String,
    pub units: String,
    pub tool_number: f64,
    /// False when `tool_number` was set to a float (`'T{:d}'` then fails).
    pub tool_number_is_int: bool,
    pub spindle_rpm: f64,
    pub spindle_dir: String,
    pub z_start: f64,
    pub z_end: f64,
    pub retract: f64,
    pub z_feed: f64,
    pub z_clear: f64,
    pub xy_feed: f64,
    pub max_spindle_rpm: Option<f64>,
    /// The machine's linear units ("MM" or "IN") when known: the retract to
    /// the top of travel is converted for a program in the other units.
    /// Not a job field.
    pub machine_units: Option<String>,
    // face and orbit ops
    pub tool_diameter: f64,
    // face
    pub step_over: f64,
    pub step_down: f64,
    pub x_start: f64,
    pub x_end: f64,
    pub y_start: f64,
    pub y_end: f64,
    // drill
    pub retract_mode: String,
    pub holes: Vec<(f64, f64)>,
    // orbit ops
    pub center: (f64, f64),
    pub diameter: f64,
    // helical bore
    pub ramp_angle: f64,
    // thread mill
    pub pitch: f64,
    pub radial_passes: f64,
    pub hand: String,
    pub pre_drill: Option<f64>,
}

fn number(key: &str, v: &Value) -> py::Result<f64> {
    py::as_number(v).ok_or_else(|| {
        Error::type_error(format!("{} must be a number, not '{}'", key, py::type_name(v)))
    })
}

fn string(key: &str, v: &Value) -> py::Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        other => Err(Error::attribute(py::type_name(other), &format!("strip ({})", key))),
    }
}

fn pair(v: &Value) -> py::Result<(f64, f64)> {
    match v {
        Value::Array(items) => {
            if items.len() > 2 {
                return Err(Error::value_error("too many values to unpack (expected 2)"));
            }
            if items.len() < 2 {
                return Err(Error::value_error(format!(
                    "not enough values to unpack (expected 2, got {})",
                    items.len()
                )));
            }
            Ok((number("x", &items[0])?, number("y", &items[1])?))
        }
        other => Err(Error::type_error(format!("'{}' object is not iterable", py::type_name(other)))),
    }
}

impl Generator {
    pub fn new(kind: Kind) -> Generator {
        Generator {
            kind,
            wcs: "G54".into(),
            coolant: "OFF".into(),
            units: "MM".into(),
            tool_number: 0.0,
            tool_number_is_int: true,
            spindle_rpm: 0.0,
            spindle_dir: "CW".into(),
            z_start: 0.0,
            z_end: 0.0,
            retract: 0.0,
            z_feed: 0.0,
            z_clear: 0.0,
            xy_feed: 0.0,
            max_spindle_rpm: None,
            machine_units: None,
            tool_diameter: 0.0,
            step_over: 0.0,
            step_down: 0.0,
            x_start: 0.0,
            x_end: 0.0,
            y_start: 0.0,
            y_end: 0.0,
            retract_mode: "G98".into(),
            holes: Vec::new(),
            center: (0.0, 0.0),
            diameter: 0.0,
            ramp_angle: 2.0,
            pitch: 0.0,
            radial_passes: 1.0,
            hand: "RH".into(),
            pre_drill: None,
        }
    }

    /// `hasattr(gen, key)` for the attributes the job stores.
    pub fn has_attr(&self, key: &str) -> bool {
        match key {
            "wcs" | "coolant" | "units" | "tool_number" | "spindle_rpm" | "spindle_dir" | "z_start"
            | "z_end" | "retract" | "z_feed" | "z_clear" | "xy_feed" | "max_spindle_rpm" => true,
            "tool_diameter" => self.kind != Kind::Drill,
            "step_over" | "step_down" | "x_start" | "x_end" | "y_start" | "y_end" => self.kind == Kind::Face,
            "retract_mode" | "holes" => self.kind == Kind::Drill,
            "center" | "diameter" => self.kind.orbit(),
            "ramp_angle" => self.kind == Kind::HelicalBore,
            "pitch" | "radial_passes" | "hand" | "pre_drill" => self.kind == Kind::ThreadMill,
            _ => false,
        }
    }

    /// The attribute as the value the job file stores for it.
    pub fn get_attr(&self, key: &str) -> Option<Value> {
        if !self.has_attr(key) {
            return None;
        }
        let f = py::float_value;
        Some(match key {
            "wcs" => Value::String(self.wcs.clone()),
            "coolant" => Value::String(self.coolant.clone()),
            "units" => Value::String(self.units.clone()),
            "spindle_dir" => Value::String(self.spindle_dir.clone()),
            "retract_mode" => Value::String(self.retract_mode.clone()),
            "hand" => Value::String(self.hand.clone()),
            "tool_number" => f(self.tool_number),
            "spindle_rpm" => f(self.spindle_rpm),
            "z_start" => f(self.z_start),
            "z_end" => f(self.z_end),
            "retract" => f(self.retract),
            "z_feed" => f(self.z_feed),
            "z_clear" => f(self.z_clear),
            "xy_feed" => f(self.xy_feed),
            "max_spindle_rpm" => self.max_spindle_rpm.map(f).unwrap_or(Value::Null),
            "tool_diameter" => f(self.tool_diameter),
            "step_over" => f(self.step_over),
            "step_down" => f(self.step_down),
            "x_start" => f(self.x_start),
            "x_end" => f(self.x_end),
            "y_start" => f(self.y_start),
            "y_end" => f(self.y_end),
            "holes" => Value::Array(self.holes.iter().map(|(x, y)| Value::Array(vec![f(*x), f(*y)])).collect()),
            "center" => Value::Array(vec![f(self.center.0), f(self.center.1)]),
            "diameter" => f(self.diameter),
            "ramp_angle" => f(self.ramp_angle),
            "pitch" => f(self.pitch),
            "radial_passes" => f(self.radial_passes),
            "pre_drill" => self.pre_drill.map(f).unwrap_or(Value::Null),
            _ => return None,
        })
    }

    /// `setattr(gen, key, value)`. Attributes the generator does not use are
    /// ignored, as they are in Python.
    pub fn set_attr(&mut self, key: &str, v: &Value) -> py::Result<()> {
        if !self.has_attr(key) {
            return Ok(());
        }
        match key {
            "wcs" => self.wcs = string(key, v)?,
            "coolant" => self.coolant = string(key, v)?,
            "units" => self.units = string(key, v)?,
            "spindle_dir" => self.spindle_dir = string(key, v)?,
            "retract_mode" => self.retract_mode = string(key, v)?,
            "hand" => self.hand = string(key, v)?,
            "tool_number" => {
                self.tool_number = number(key, v)?;
                self.tool_number_is_int = !matches!(v, Value::Number(n) if n.is_f64());
            }
            "spindle_rpm" => self.spindle_rpm = number(key, v)?,
            "z_start" => self.z_start = number(key, v)?,
            "z_end" => self.z_end = number(key, v)?,
            "retract" => self.retract = number(key, v)?,
            "z_feed" => self.z_feed = number(key, v)?,
            "z_clear" => self.z_clear = number(key, v)?,
            "xy_feed" => self.xy_feed = number(key, v)?,
            "max_spindle_rpm" => {
                self.max_spindle_rpm = if v.is_null() { None } else { Some(number(key, v)?) }
            }
            "tool_diameter" => self.tool_diameter = number(key, v)?,
            "step_over" => self.step_over = number(key, v)?,
            "step_down" => self.step_down = number(key, v)?,
            "x_start" => self.x_start = number(key, v)?,
            "x_end" => self.x_end = number(key, v)?,
            "y_start" => self.y_start = number(key, v)?,
            "y_end" => self.y_end = number(key, v)?,
            "holes" => match v {
                Value::Array(items) => self.holes = items.iter().map(pair).collect::<py::Result<_>>()?,
                other => return Err(Error::type_error(format!("'{}' object is not iterable", py::type_name(other)))),
            },
            "center" => self.center = pair(v)?,
            "diameter" => self.diameter = number(key, v)?,
            "ramp_angle" => self.ramp_angle = number(key, v)?,
            "pitch" => self.pitch = number(key, v)?,
            "radial_passes" => self.radial_passes = number(key, v)?,
            "pre_drill" => self.pre_drill = if v.is_null() { None } else { Some(number(key, v)?) },
            _ => {}
        }
        Ok(())
    }

    fn tool_d(&self) -> py::Result<String> {
        if !self.tool_number_is_int {
            return Err(Error::value_error("Unknown format code 'd' for object of type 'float'"));
        }
        Ok(format!("{}", self.tool_number as i64))
    }

    // -- validation -----------------------------------------------------------

    fn validate_base(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.tool_number <= 0.0 {
            errors.push("Choose a tool from the tool table (T1 or higher).".into());
        }
        if !(self.spindle_rpm > 0.0) {
            errors.push("Spindle RPM must be greater than 0.".into());
        } else if let Some(max) = self.max_spindle_rpm.filter(|m| *m != 0.0) {
            if self.spindle_rpm > max {
                errors.push(format!(
                    "Spindle RPM {} is above the machine maximum of {}.",
                    general(self.spindle_rpm),
                    general(max)
                ));
            }
        }
        if !(self.z_start > self.z_end) {
            errors.push("Z start must be above Z end.".into());
        }
        if self.retract < 0.0 {
            errors.push("Retract height cannot be negative.".into());
        }
        let r_plane = self.z_start + self.retract;
        if !(self.z_clear > r_plane) {
            errors.push(format!(
                "Clearance height ({}) must be above Z start + retract ({}).",
                f4(self.z_clear),
                f4(r_plane)
            ));
        }
        errors
    }

    fn validate_face(&self) -> Vec<String> {
        let mut errors = self.validate_base();
        if !(self.tool_diameter > 0.0) {
            errors.push("The tool table has no diameter for this tool.".into());
        }
        if !(self.xy_feed > 0.0) {
            errors.push("XY feed rate must be greater than 0.".into());
        }
        if !(self.step_down > 0.0) {
            errors.push("Step down must be greater than 0.".into());
        }
        if !(self.step_over > 0.0) {
            errors.push("Step over must be greater than 0.".into());
        } else if self.tool_diameter > 0.0 && self.step_over > self.tool_diameter {
            errors.push(format!(
                "Step over ({}) is wider than the tool ({}) and would leave uncut ridges.",
                f4(self.step_over),
                f4(self.tool_diameter)
            ));
        }
        if !(self.x_start < self.x_end) {
            errors.push("X start must be less than X end.".into());
        }
        if !(self.y_start > self.y_end) {
            errors.push("Y start must be greater than Y end.".into());
        }
        errors
    }

    pub fn validate_drill(&self, cycle: &str, param: f64) -> Vec<String> {
        let mut errors = self.validate_base();
        if self.holes.is_empty() {
            errors.push("Add at least one hole position.".into());
        }
        let tap = cycle == "TAP" || cycle == "RIGID TAP";
        if !tap && !(self.z_feed > 0.0) {
            errors.push("Z feed rate must be greater than 0.".into());
        }
        if self.retract_mode == "G99" && !(self.retract > 0.0) {
            errors.push("G99 rapids between holes at Z start + retract, so retract must be greater than 0.".into());
        }
        if (cycle == "PECK" || cycle == "BREAK") && !(param > 0.0) {
            errors.push(format!("{} depth must be greater than 0.", py::title(cycle)));
        }
        if cycle == "DWELL" && param < 0.0 {
            errors.push("Dwell time cannot be negative.".into());
        }
        if tap && !(param > 0.0) {
            errors.push("Thread pitch must be greater than 0.".into());
        }
        if !DRILL_TYPES.contains(&cycle) {
            errors.push(format!("Unknown drill cycle {}.", cycle));
        }
        errors
    }

    fn validate_orbit(&self) -> Vec<String> {
        let mut errors = self.validate_base();
        if !(self.tool_diameter > 0.0) {
            errors.push("The tool table has no diameter for this tool.".into());
        }
        if !(self.xy_feed > 0.0) {
            errors.push("Feed rate must be greater than 0.".into());
        }
        if py::strip_upper(&self.spindle_dir) != "CW" {
            errors.push("This is a climb cut and needs CW spindle direction.".into());
        }
        if self.tool_diameter > 0.0 && !(self.diameter > self.tool_diameter) {
            errors.push(format!(
                "Diameter ({}) must be larger than the tool ({}).",
                f4(self.diameter),
                f4(self.tool_diameter)
            ));
        }
        errors
    }

    fn validate_helical(&self) -> Vec<String> {
        let mut errors = self.validate_orbit();
        if !(0.0 < self.ramp_angle && self.ramp_angle <= MAX_RAMP_ANGLE) {
            errors.push(format!(
                "Ramp angle must be more than 0 and at most {} degrees.",
                general(MAX_RAMP_ANGLE)
            ));
        }
        errors
    }

    fn validate_thread(&self) -> Vec<String> {
        let mut errors = self.validate_orbit();
        if !(self.pitch > 0.0) {
            errors.push("Pitch must be greater than 0.".into());
        }
        if self.hand != "RH" && self.hand != "LH" {
            errors.push("Thread hand must be RH or LH.".into());
        }
        if (self.radial_passes.trunc() as i64) < 1 {
            errors.push("Radial passes must be at least 1.".into());
        }
        if self.tool_diameter > 0.0 && self.pitch > 0.0 && !(self.tool_diameter < self.minor_diameter()) {
            errors.push(format!(
                "Tool ({}) must be smaller than the thread minor diameter ({}).",
                f4(self.tool_diameter),
                f4(self.minor_diameter())
            ));
        }
        if self.pitch > 0.0 && self.z_start > self.z_end && !(self.z_clear > self.z_start + self.pitch) {
            errors.push("Clearance height must be more than one pitch above Z start.".into());
        }
        errors
    }

    fn check(errors: Vec<String>) -> GResult<()> {
        if errors.is_empty() {
            Ok(())
        } else {
            Err(GenError::Operation(errors))
        }
    }

    // -- program blocks ---------------------------------------------------------

    fn coolant_on(&self) -> Vec<String> {
        match py::strip_upper(&self.coolant).as_str() {
            "MIST" => vec!["M7".into()],
            "FLOOD" => vec!["M8".into()],
            _ => vec![],
        }
    }

    /// `_start_op`: tool change, spindle, WCS and a safe approach.
    pub fn start_op(&self, first: (f64, f64)) -> py::Result<Vec<String>> {
        let tool = self.tool_d()?;
        let mut g = vec![
            if py::strip_upper(&self.units) == "IN" { "G20".to_string() } else { "G21".to_string() },
            self.top(),
            format!("T{} M6", tool),
            format!("G43 H{}", tool),
            self.wcs.clone(),
            format!(
                "S{} {}",
                f4(self.spindle_rpm),
                if py::strip_upper(&self.spindle_dir) == "CCW" { "M4" } else { "M3" }
            ),
        ];
        g.extend(self.coolant_on());
        g.push(format!("F{}", f4(self.xy_feed)));
        g.push(format!("G0 X{} Y{}", f4(first.0), f4(first.1)));
        g.push(format!("G0 Z{}", f4(self.z_clear)));
        Ok(g)
    }

    /// The retract to the top of travel in this op's units.
    pub fn top(&self) -> String {
        machine_top_in(self.machine_units.as_deref(), &self.units)
    }

    /// `_end_op`.
    pub fn end_op(&self) -> Vec<String> {
        let mut g = vec![format!("G0 Z{}", f4(self.z_clear))];
        if !self.coolant_on().is_empty() {
            g.push("M9".into());
        }
        g.push("M5".into());
        g
    }

    // -- face -------------------------------------------------------------------

    pub fn pass_counts(&self) -> py::Result<(i64, i64)> {
        let width = (self.y_end - self.y_start).abs();
        let depth = (self.z_end - self.z_start).abs();
        Ok((
            1.max(py::ceil_int(depth / self.step_down - 1e-9)?),
            1.max(py::ceil_int(width / self.step_over - 1e-9)?),
        ))
    }

    pub fn face(&self) -> GResult<Vec<String>> {
        Self::check(self.validate_face())?;
        let (num_step_down, num_step_over) = self.pass_counts()?;
        let width = (self.y_end - self.y_start).abs();
        let step_over = width / num_step_over as f64;
        let step_down = (self.z_end - self.z_start).abs() / num_step_down as f64;
        let tool_radius = self.tool_diameter / 2.0;
        let ramp_radius = self.retract + step_down;
        let x_approach = self.x_start - tool_radius - ramp_radius;
        let y_first = self.y_start + tool_radius - step_over;

        let mut g = self.start_op((x_approach, y_first))?;
        let mut z = self.z_start;
        for i in 0..num_step_down {
            if i > 0 {
                g.push(format!("G0 Z{}", f4(self.z_clear)));
                g.push(format!("G0 X{} Y{}", f4(x_approach), f4(y_first)));
            }
            g.push(format!("G0 Z{}", f4(z + self.retract)));
            g.push(format!(
                "G18 G2 X{} Z{} I{}",
                f4(x_approach + ramp_radius),
                f4(z + self.retract - ramp_radius),
                f4(ramp_radius)
            ));
            z -= step_down;
            let mut x = self.x_end;
            let mut y = y_first;
            g.push(format!("G1 X{}", f4(x)));
            for _ in 0..(num_step_over - 1) {
                y -= step_over;
                if x == self.x_end {
                    g.push(format!("G17 G2 Y{} J{}", f4(y), f4(-step_over / 2.0)));
                    x = self.x_start;
                } else {
                    g.push(format!("G17 G3 Y{} J{}", f4(y), f4(-step_over / 2.0)));
                    x = self.x_end;
                }
                g.push(format!("G1 X{}", f4(x)));
            }
            if x == self.x_start {
                g.push(format!("G18 G3 X{} Z{} K{}", f4(x - ramp_radius), f4(z + ramp_radius), f4(ramp_radius)));
            } else {
                g.push(format!("G18 G2 X{} Z{} K{}", f4(x + ramp_radius), f4(z + ramp_radius), f4(ramp_radius)));
            }
            g.push("G17".into());
        }
        g.extend(self.end_op());
        Ok(g)
    }

    // -- drill ------------------------------------------------------------------

    pub fn drill(&self, cycle: &str, param: f64) -> GResult<Vec<String>> {
        Self::check(self.validate_drill(cycle, param))?;
        if cycle == "MANUAL" {
            return Ok(self.manual()?);
        }
        if cycle == "RIGID TAP" {
            return Ok(self.rigid_tap(param)?);
        }
        let r_plane = f4(self.z_start + self.retract);
        let z_end = f4(self.z_end);
        let block = match cycle {
            "PECK" => format!("G83 R{} Z{} Q{} F{}", r_plane, z_end, f4(param), f4(self.z_feed)),
            "BREAK" => format!("G73 R{} Z{} Q{} F{}", r_plane, z_end, f4(param), f4(self.z_feed)),
            "DWELL" => format!("G82 R{} Z{} P{} F{}", r_plane, z_end, f4(param), f4(self.z_feed)),
            "TAP" => {
                let tap = if py::strip_upper(&self.spindle_dir) == "CCW" { "G74" } else { "G84" };
                format!("{} R{} Z{} F{}", tap, r_plane, z_end, f4((self.spindle_rpm * param).abs()))
            }
            _ => format!("G81 R{} Z{} F{}", r_plane, z_end, f4(self.z_feed)),
        };
        let mut g = self.start_op(self.holes[0])?;
        g.push(format!("{} {}", self.retract_mode, block));
        for (x, y) in &self.holes[1..] {
            g.push(format!("X{} Y{}", f4(*x), f4(*y)));
        }
        g.push("G80".into());
        g.extend(self.end_op());
        Ok(g)
    }

    fn travel_height(&self) -> f64 {
        if self.retract_mode == "G99" {
            self.z_start + self.retract
        } else {
            self.z_clear
        }
    }

    fn manual(&self) -> py::Result<Vec<String>> {
        let mut g = self.start_op(self.holes[0])?;
        let n = self.holes.len();
        for (index, (x, y)) in self.holes.iter().enumerate() {
            if index > 0 {
                g.push(format!("G0 X{} Y{}", f4(*x), f4(*y)));
            }
            g.push(format!("G0 Z{}", f4(self.z_start + self.retract)));
            g.push(format!("M0 (hole {} of {})", index + 1, n));
            g.push(format!("G0 Z{}", f4(self.travel_height())));
        }
        g.extend(self.end_op());
        Ok(g)
    }

    fn rigid_tap(&self, pitch: f64) -> py::Result<Vec<String>> {
        let mut g = self.start_op(self.holes[0])?;
        for (index, (x, y)) in self.holes.iter().enumerate() {
            if index > 0 {
                g.push(format!("G0 X{} Y{}", f4(*x), f4(*y)));
            }
            g.push(format!("G0 Z{}", f4(self.z_start + self.retract)));
            g.push(format!("G33.1 Z{} K{}", f4(self.z_end), f4(pitch)));
            g.push(format!("G0 Z{}", f4(self.travel_height())));
        }
        g.extend(self.end_op());
        Ok(g)
    }

    // -- orbit ops ----------------------------------------------------------------

    pub fn final_radius(&self) -> f64 {
        (self.diameter - self.tool_diameter) / 2.0
    }

    fn centre_feed(&self, radius: f64) -> f64 {
        self.xy_feed * radius / (radius + self.tool_diameter / 2.0)
    }

    fn orbit_line(&self, radius: f64, z: f64) -> String {
        let (cx, cy) = self.center;
        format!("G3 X{} Y{} Z{} I{} J0", f4(cx + radius), f4(cy), f4(z), f4(-radius))
    }

    pub fn helical_pass_radii(&self) -> py::Result<Vec<f64>> {
        let fin = self.final_radius();
        let passes = 1.max(py::ceil_int(fin / (self.tool_diameter / 2.0) - 1e-9)?);
        Ok((1..=passes).map(|k| fin * k as f64 / passes as f64).collect())
    }

    pub fn pitch_for_radius(&self, radius: f64) -> f64 {
        let pitch = 2.0 * std::f64::consts::PI * radius * self.ramp_angle.to_radians().tan();
        if self.tool_diameter / 2.0 < pitch {
            self.tool_diameter / 2.0
        } else {
            pitch
        }
    }

    pub fn helical(&self) -> GResult<Vec<String>> {
        Self::check(self.validate_helical())?;
        let (cx, cy) = self.center;
        let r_plane = self.z_start + self.retract;
        let mut g = self.start_op(self.center)?;
        for radius in self.helical_pass_radii()? {
            let pitch = self.pitch_for_radius(radius);
            g.push(format!("G0 Z{}", f4(r_plane)));
            g.push(format!("G1 X{} Y{} F{}", f4(cx + radius), f4(cy), f4(self.centre_feed(radius))));
            let turns = 1.max(py::ceil_int((r_plane - self.z_end) / pitch - 1e-9)?);
            let step = (r_plane - self.z_end) / turns as f64;
            let mut z = r_plane;
            for _ in 0..turns {
                z -= step;
                g.push(self.orbit_line(radius, z));
            }
            g.push(self.orbit_line(radius, self.z_end));
            g.push(format!("G1 X{} Y{}", f4(cx), f4(cy)));
            g.push(format!("G0 Z{}", f4(self.z_clear)));
        }
        g.extend(self.end_op());
        Ok(g)
    }

    pub fn thread_depth(&self) -> f64 {
        THREAD_DEPTH_PER_PITCH * self.pitch
    }

    pub fn minor_diameter(&self) -> f64 {
        self.diameter - 2.0 * self.thread_depth()
    }

    pub fn thread_pass_radii(&self) -> Vec<f64> {
        let fin = self.final_radius();
        let depth = self.thread_depth();
        let passes = 1.max(self.radial_passes.trunc() as i64);
        (1..=passes).map(|k| fin - depth * (passes - k) as f64 / passes as f64).collect()
    }

    pub fn thread(&self) -> GResult<Vec<String>> {
        Self::check(self.validate_thread())?;
        let (cx, cy) = self.center;
        let span = self.z_start - self.z_end;
        let turns = 1.max(py::ceil_int(span / self.pitch - 1e-9)?);
        let (z_begin, direction) = if self.hand == "RH" {
            (self.z_end, 1.0)
        } else {
            (self.z_end + turns as f64 * self.pitch, -1.0)
        };
        let mut g = self.start_op(self.center)?;
        for radius in self.thread_pass_radii() {
            let feed = self.centre_feed(radius);
            g.push(format!("G0 Z{}", f4(self.z_start + self.retract)));
            g.push(format!("G1 Z{} F{}", f4(z_begin), f4(self.xy_feed)));
            g.push(format!("G1 X{} Y{} F{}", f4(cx + radius), f4(cy), f4(feed / 2.0)));
            g.push(format!("F{}", f4(feed)));
            let mut z = z_begin;
            for _ in 0..turns {
                z += direction * self.pitch;
                g.push(self.orbit_line(radius, z));
            }
            g.push(format!("G1 X{} Y{}", f4(cx), f4(cy)));
            g.push(format!("G0 Z{}", f4(self.z_clear)));
        }
        g.extend(self.end_op());
        Ok(g)
    }

    // -- entry points ---------------------------------------------------------------

    /// `face()` / `generate(cycle, param)` / `generate()` by kind.
    pub fn lines(&self, cycle: &str, param: f64) -> GResult<Vec<String>> {
        match self.kind {
            Kind::Face => self.face(),
            Kind::Drill => self.drill(cycle, param),
            Kind::HelicalBore => self.helical(),
            Kind::ThreadMill => self.thread(),
        }
    }

    /// `summary()` (drill: `summary(cycle)`).
    pub fn summary(&self, cycle: &str) -> py::Result<String> {
        let tool = self.tool_d()?;
        Ok(match self.kind {
            Kind::Face => {
                let (downs, overs) = self.pass_counts()?;
                format!(
                    "Facing {} x {} with T{}: {} depth pass(es) x {} stepover pass(es).",
                    f4((self.x_end - self.x_start).abs()),
                    f4((self.y_start - self.y_end).abs()),
                    tool,
                    downs,
                    overs
                )
            }
            Kind::Drill => format!(
                "{} {} hole(s) with T{} from Z{} to Z{}.",
                py::title(cycle),
                self.holes.len(),
                tool,
                f4(self.z_start),
                f4(self.z_end)
            ),
            Kind::HelicalBore => {
                let radii = self.helical_pass_radii()?;
                format!(
                    "Helical bore {} from Z{} to Z{} with T{}: {} radial pass(es), {} degree ramp ({} per turn on the final pass).",
                    f4(self.diameter),
                    f4(self.z_start),
                    f4(self.z_end),
                    tool,
                    radii.len(),
                    general(self.ramp_angle),
                    f4(self.pitch_for_radius(*radii.last().unwrap()))
                )
            }
            Kind::ThreadMill => {
                let drill = match self.pre_drill {
                    Some(d) if d != 0.0 => d,
                    _ => self.minor_diameter(),
                };
                format!(
                    "Thread mill {} x {} {} from Z{} to Z{} with T{}: {} radial pass(es). Pre-drill {} below Z{} first.",
                    f4(self.diameter),
                    f4(self.pitch),
                    self.hand,
                    f4(self.z_start),
                    f4(self.z_end),
                    tool,
                    self.thread_pass_radii().len(),
                    f4(drill),
                    f4(self.z_end)
                )
            }
        })
    }
}

/// `program_lines`: the standard preamble and epilog, numbered.
pub fn program_lines(ops: &[Vec<String>]) -> Vec<String> {
    program_lines_to(ops, &machine_top())
}

/// `program_lines` ending with the retract `top` (`Generator::top`, in the
/// units the ops leave in force).
pub fn program_lines_to(ops: &[Vec<String>], top: &str) -> Vec<String> {
    let mut lines = vec!["G90 G94 G17 G40 G49 G80 G91.1".to_string()];
    for op in ops {
        lines.extend(op.iter().cloned());
    }
    lines.push(top.to_string());
    lines.push("M30".into());
    lines.iter().enumerate().map(|(i, l)| format!("N{} {}", (i + 1) * 10, l)).collect()
}

pub fn program_text(ops: &[Vec<String>]) -> String {
    program_lines(ops).join("\n") + "\n"
}

/// `program_text` ending with the retract `top`.
pub fn program_text_to(ops: &[Vec<String>], top: &str) -> String {
    program_lines_to(ops, top).join("\n") + "\n"
}

// -- thread presets ---------------------------------------------------------------

pub const MM_PER_INCH: f64 = 25.4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreadPreset {
    pub name: &'static str,
    pub major: f64,
    pub pitch: f64,
    pub tap_drill: f64,
    pub units: &'static str,
}

const fn metric(name: &'static str, major: f64, pitch: f64, drill: f64) -> ThreadPreset {
    ThreadPreset { name, major, pitch, tap_drill: drill, units: "MM" }
}

fn un(name: &'static str, major: f64, tpi: f64, drill: f64) -> ThreadPreset {
    ThreadPreset { name, major, pitch: 1.0 / tpi, tap_drill: drill, units: "IN" }
}

pub fn presets() -> Vec<ThreadPreset> {
    vec![
        metric("M2 x 0.4", 2.0, 0.4, 1.6),
        metric("M2.5 x 0.45", 2.5, 0.45, 2.05),
        metric("M3 x 0.5", 3.0, 0.5, 2.5),
        metric("M4 x 0.7", 4.0, 0.7, 3.3),
        metric("M5 x 0.8", 5.0, 0.8, 4.2),
        metric("M6 x 1", 6.0, 1.0, 5.0),
        metric("M8 x 1.25", 8.0, 1.25, 6.8),
        metric("M8 x 1 FINE", 8.0, 1.0, 7.0),
        metric("M10 x 1.5", 10.0, 1.5, 8.5),
        metric("M10 x 1.25 FINE", 10.0, 1.25, 8.8),
        metric("M12 x 1.75", 12.0, 1.75, 10.2),
        metric("M12 x 1.5 FINE", 12.0, 1.5, 10.5),
        metric("M14 x 2", 14.0, 2.0, 12.0),
        metric("M16 x 2", 16.0, 2.0, 14.0),
        metric("M16 x 1.5 FINE", 16.0, 1.5, 14.5),
        metric("M20 x 2.5", 20.0, 2.5, 17.5),
        metric("M24 x 3", 24.0, 3.0, 21.0),
        un("#4-40 UNC", 0.112, 40.0, 0.0890),
        un("#6-32 UNC", 0.138, 32.0, 0.1065),
        un("#8-32 UNC", 0.164, 32.0, 0.1360),
        un("#10-24 UNC", 0.190, 24.0, 0.1495),
        un("#10-32 UNF", 0.190, 32.0, 0.1590),
        un("1/4-20 UNC", 0.250, 20.0, 0.2010),
        un("1/4-28 UNF", 0.250, 28.0, 0.2130),
        un("5/16-18 UNC", 0.3125, 18.0, 0.2570),
        un("5/16-24 UNF", 0.3125, 24.0, 0.2720),
        un("3/8-16 UNC", 0.375, 16.0, 0.3125),
        un("3/8-24 UNF", 0.375, 24.0, 0.3320),
        un("7/16-14 UNC", 0.4375, 14.0, 0.3680),
        un("1/2-13 UNC", 0.500, 13.0, 0.4219),
        un("1/2-20 UNF", 0.500, 20.0, 0.4531),
        un("5/8-11 UNC", 0.625, 11.0, 0.5312),
        un("3/4-10 UNC", 0.750, 10.0, 0.6562),
        un("1-8 UNC", 1.000, 8.0, 0.8750),
    ]
}

pub fn find_preset(name: &Value) -> Option<ThreadPreset> {
    presets().into_iter().find(|p| matches!(name, Value::String(s) if s == p.name))
}

pub fn convert(value: f64, from_units: &str, to_units: &str) -> f64 {
    let from = py::strip_upper(from_units);
    let to = py::strip_upper(to_units);
    if from == to {
        value
    } else if from == "IN" {
        value * MM_PER_INCH
    } else {
        value / MM_PER_INCH
    }
}

/// `(major diameter, pitch, tap drill)` in `units`.
pub fn preset_values(p: &ThreadPreset, units: &str) -> (f64, f64, f64) {
    (convert(p.major, p.units, units), convert(p.pitch, p.units, units), convert(p.tap_drill, p.units, units))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn common(g: &mut Generator) {
        g.wcs = "G54".into();
        g.coolant = "FLOOD".into();
        g.tool_number = 3.0;
        g.spindle_rpm = 1200.0;
        g.z_end = -2.0;
        g.retract = 2.0;
        g.z_feed = 100.0;
        g.z_clear = 10.0;
        g.xy_feed = 400.0;
        g.max_spindle_rpm = Some(4250.0);
    }

    fn face_op() -> Generator {
        let mut f = Generator::new(Kind::Face);
        common(&mut f);
        f.tool_diameter = 50.0;
        f.step_over = 35.0;
        f.step_down = 1.0;
        f.x_end = 100.0;
        f.y_start = 60.0;
        f
    }

    fn drill_op() -> Generator {
        let mut d = Generator::new(Kind::Drill);
        common(&mut d);
        d.holes = vec![(0.0, 0.0), (20.0, 0.0), (20.0, 20.0)];
        d
    }

    fn ops_error(r: GResult<Vec<String>>) -> String {
        match r {
            Err(GenError::Operation(e)) => e.join("\n"),
            other => panic!("expected OperationError, got {:?}", other),
        }
    }

    fn orbit(kind: Kind) -> Generator {
        let mut h = Generator::new(kind);
        common(&mut h);
        h.center = (10.0, 20.0);
        h.tool_diameter = 6.0;
        h.diameter = 10.0;
        h.z_end = -12.0;
        h.xy_feed = 300.0;
        if kind == Kind::ThreadMill {
            h.pitch = 1.5;
            h.radial_passes = 2.0;
        }
        h
    }

    #[test]
    fn program_lifts_before_tool_change_and_first_rapid() {
        let lines = face_op().face().unwrap();
        let top = lines.iter().position(|l| l == "G53 G0 Z0").unwrap();
        let tc = lines.iter().position(|l| l == "T3 M6").unwrap();
        let first = lines.iter().position(|l| l.starts_with("G0 X")).unwrap();
        assert!(top < tc && tc < first);
        assert_eq!(lines[first + 1], "G0 Z10.0000");
        assert_eq!(lines.last().unwrap(), "M5");
    }

    #[test]
    fn face_pass_counts() {
        let mut f = face_op();
        f.z_end = -3.0;
        assert_eq!(f.pass_counts().unwrap(), (3, 2));
    }

    #[test]
    fn face_rejects_unsafe_inputs() {
        let cases: Vec<(Box<dyn Fn(&mut Generator)>, &str)> = vec![
            (Box::new(|f| f.z_clear = 1.0), "Clearance height"),
            (Box::new(|f| f.step_over = 60.0), "wider than the tool"),
            (Box::new(|f| f.step_down = 0.0), "Step down"),
            (Box::new(|f| f.spindle_rpm = 5000.0), "machine maximum"),
            (Box::new(|f| f.tool_diameter = 0.0), "diameter"),
            (Box::new(|f| { f.x_start = 100.0; f.x_end = 0.0 }), "X start"),
        ];
        for (mutate, message) in cases {
            let mut f = face_op();
            mutate(&mut f);
            assert!(ops_error(f.face()).contains(message), "{}", message);
        }
    }

    #[test]
    fn canned_cycle_and_rigid_tap() {
        let lines = drill_op().drill("PECK", 0.5).unwrap();
        let i = lines.iter().position(|l| l.contains("G83")).unwrap();
        assert_eq!(lines[i - 1], "G0 Z10.0000");
        assert_eq!(lines[i], "G98 G83 R2.0000 Z-2.0000 Q0.5000 F100.0000");
        assert_eq!(lines[i + 1..i + 3], ["X20.0000 Y0.0000", "X20.0000 Y20.0000"]);
        assert_eq!(lines[i + 3], "G80");
        let lines = drill_op().drill("RIGID TAP", 1.25).unwrap();
        let taps: Vec<_> = lines.iter().enumerate().filter(|(_, l)| l.starts_with("G33.1")).collect();
        assert_eq!(taps.len(), 3);
        for (i, l) in taps {
            assert_eq!(l, "G33.1 Z-2.0000 K1.2500");
            assert_eq!(lines[i - 1], "G0 Z2.0000");
        }
        let mut d = drill_op();
        d.spindle_rpm = 400.0;
        assert!(d.drill("TAP", 1.5).unwrap().contains(&"G98 G84 R2.0000 Z-2.0000 F600.0000".to_string()));
    }

    #[test]
    fn drill_rejects_unsafe_inputs() {
        assert!(ops_error(drill_op().drill("PECK", 0.0)).contains("Peck depth"));
        assert!(ops_error(drill_op().drill("BREAK", 0.0)).contains("Break depth"));
        assert!(ops_error(drill_op().drill("TAP", 0.0)).contains("pitch"));
        assert!(ops_error(drill_op().drill("DWELL", -1.0)).contains("Dwell"));
        let mut d = drill_op();
        d.retract_mode = "G99".into();
        d.retract = 0.0;
        assert!(ops_error(d.drill("DRILL", 0.0)).contains("G99"));
        let mut d = drill_op();
        d.holes.clear();
        assert!(ops_error(d.drill("DRILL", 0.0)).contains("at least one hole"));
    }

    #[test]
    fn program_text_wraps_ops() {
        let text = program_text(&[drill_op().drill("DRILL", 0.0).unwrap()]);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0], "N10 G90 G94 G17 G40 G49 G80 G91.1");
        let n = lines.len();
        assert_eq!(lines[n - 2], format!("N{} G53 G0 Z0", n * 10 - 10));
        assert_eq!(lines[n - 1], format!("N{} M30", n * 10));
    }

    #[test]
    fn helical_bore_geometry() {
        let mut h = orbit(Kind::HelicalBore);
        h.diameter = 20.0;
        let radii = h.helical_pass_radii().unwrap();
        assert_eq!(radii.len(), 3);
        assert!((radii[2] - 7.0).abs() < 1e-12);
        let lines = h.helical().unwrap();
        assert_eq!(lines.iter().filter(|l| *l == "G1 X10.0000 Y20.0000").count(), 3);
        let mut steep = orbit(Kind::HelicalBore);
        steep.ramp_angle = 10.0;
        assert!((steep.pitch_for_radius(7.0) - 3.0).abs() < 1e-12);
        let lead = orbit(Kind::HelicalBore).helical().unwrap();
        assert!(lead.iter().find(|l| l.starts_with("G1 X12.0000")).unwrap().ends_with("F120.0000"));
    }

    #[test]
    fn thread_directions() {
        let lines = orbit(Kind::ThreadMill).thread().unwrap();
        assert_eq!(lines.iter().filter(|l| l.starts_with("G3 ")).count(), 16);
        let descents: Vec<_> = lines.iter().filter(|l| l.starts_with("G1 Z")).collect();
        assert_eq!(descents, vec!["G1 Z-12.0000 F300.0000"; 2]);
        let mut lh = orbit(Kind::ThreadMill);
        lh.hand = "LH".into();
        let lines = lh.thread().unwrap();
        assert_eq!(lines.iter().find(|l| l.starts_with("G1 Z")).unwrap(), "G1 Z0.0000 F300.0000");
    }

    #[test]
    fn orbit_ops_reject_unsafe_inputs() {
        let mut t = orbit(Kind::ThreadMill);
        t.tool_diameter = 9.0;
        assert!(ops_error(t.thread()).contains("minor diameter"));
        let mut t = orbit(Kind::ThreadMill);
        t.spindle_dir = "CCW".into();
        assert!(ops_error(t.thread()).contains("CW spindle"));
        let mut h = orbit(Kind::HelicalBore);
        h.ramp_angle = 15.0;
        assert!(ops_error(h.helical()).contains("Ramp angle must be more than 0 and at most 10 degrees."));
        let mut h = orbit(Kind::HelicalBore);
        h.diameter = 5.0;
        assert!(ops_error(h.helical()).contains("larger than the tool"));
    }

    #[test]
    fn the_top_of_travel_in_the_program_units() {
        let t = |top: &str, m: Option<&str>, u: &str| top_in(top.to_string(), m, u);
        assert_eq!(t("G53 G0 Z0", Some("IN"), "MM"), "G53 G0 Z0");
        assert_eq!(t("G53 G0 Z3.9803", Some("IN"), "IN"), "G53 G0 Z3.9803");
        assert_eq!(t("G53 G0 Z3.9803", Some("IN"), "MM"), "G53 G0 Z101.0996");
        assert_eq!(t("G53 G0 Z99.5", Some("MM"), "IN"), "G53 G0 Z3.9173");
        // Machine units unknown: as before.
        assert_eq!(t("G53 G0 Z99.5", None, "IN"), "G53 G0 Z99.5");
        let mut g = face_op();
        g.units = "IN".into();
        g.machine_units = Some("IN".into());
        assert_eq!(g.top(), machine_top());
    }

    #[test]
    fn presets_convert() {
        let p = find_preset(&Value::String("1/4-20 UNC".into())).unwrap();
        let (major, pitch, drill) = preset_values(&p, "MM");
        assert!((major - 6.35).abs() < 1e-9 && (pitch - 1.27).abs() < 1e-9 && (drill - 5.1054).abs() < 1e-9);
        for p in presets() {
            let (major, pitch, drill) = preset_values(&p, "MM");
            let minor = major - 2.0 * THREAD_DEPTH_PER_PITCH * pitch;
            assert!(minor - 0.05 <= drill && drill < major, "{}", p.name);
        }
    }
}
