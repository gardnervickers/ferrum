//! What millgui reads about the machine from the INI and never asks for:
//! linear units, the axes and the joints behind them, which axes are rotary
//! and whether they have a home switch, the jog increments, and the top of
//! Z travel. Read once at startup (`init`); every screen asks `get()`.
//!
//! Positions, velocities and limits from LinuxCNC are in the machine's
//! units ([TRAJ]LINEAR_UNITS), so millgui only needs the units for what it
//! adds itself: labels, decimals, and its own starting values, which are
//! written in millimetres and converted with `Units::from_mm`.

use crate::config::Config;
use machine::{Status, AXIS_LETTERS, JOINTS};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Units {
    #[default]
    Mm,
    Inch,
}

impl Units {
    /// [TRAJ]LINEAR_UNITS: a name (`mm`, `metric`, `inch`, `imperial`, `in`,
    /// `cm`) or a number of units per mm (1 = mm, 0.03937 = inch).
    pub fn parse(v: &str) -> Option<Units> {
        let v = v.trim().to_ascii_lowercase();
        match v.as_str() {
            "mm" | "metric" | "millimeter" | "millimetre" => Some(Units::Mm),
            "in" | "inch" | "inches" | "imperial" => Some(Units::Inch),
            _ => {
                let n: f64 = v.parse().ok()?;
                if (n - 1.0).abs() < 1e-6 {
                    Some(Units::Mm)
                } else if (n - 1.0 / 25.4).abs() < 1e-6 {
                    Some(Units::Inch)
                } else {
                    None
                }
            }
        }
    }

    pub fn inch(self) -> bool {
        self == Units::Inch
    }

    /// A length given in millimetres, in these units.
    pub fn from_mm(self, mm: f64) -> f64 {
        match self {
            Units::Mm => mm,
            Units::Inch => mm / 25.4,
        }
    }

    /// A starting value: `mm` on a metric machine, `inch` on an inch one
    /// (round numbers rather than converted ones).
    pub fn pick(self, mm: f64, inch: f64) -> f64 {
        match self {
            Units::Mm => mm,
            Units::Inch => inch,
        }
    }

    /// "mm" or "in".
    pub fn name(self) -> &'static str {
        match self {
            Units::Mm => "mm",
            Units::Inch => "in",
        }
    }

    /// "MM" or "IN", for labels in capitals.
    pub fn upper(self) -> &'static str {
        match self {
            Units::Mm => "MM",
            Units::Inch => "IN",
        }
    }

    /// Decimal places for a position: 3 in mm, 4 in inches.
    pub fn decimals(self) -> usize {
        match self {
            Units::Mm => 3,
            Units::Inch => 4,
        }
    }

    /// A length to the unit's decimals.
    pub fn fmt(self, v: f64) -> String {
        format!("{v:.*}", self.decimals())
    }

    /// A length for G-code: the unit's decimals, without trailing zeros.
    pub fn gcode(self, v: f64) -> String {
        let s = format!("{v:.*}", self.decimals() + 1);
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s == "-0" { "0".into() } else { s.to_string() }
    }
}

/// One axis letter in [TRAJ]COORDINATES.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub letter: char,
    /// Index in `AXIS_LETTERS` (X 0 ... W 8): positions and offsets use it.
    pub index: usize,
    /// The joints that move this axis (two for a gantry). Empty when the
    /// kinematics are not trivkins and millgui cannot tell.
    pub joints: Vec<usize>,
    pub rotary: bool,
    /// A joint has a home switch to search for ([JOINT_n]HOME_SEARCH_VEL is
    /// not 0). Without one, homing sets the reference where the axis stands.
    pub home_switch: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MachineInfo {
    pub units: Units,
    /// The INI's LINEAR_UNITS value when millgui could not read it (and
    /// used millimetres instead).
    pub units_unknown: Option<String>,
    pub axes: Vec<Axis>,
    /// trivkins: each axis has its own joints, which can be homed and
    /// jogged one by one. Otherwise only HOME ALL is offered and nothing
    /// jogs before homing.
    pub trivial_kins: bool,
    pub joints: usize,
    /// Machine Z that millgui retracts to (the top of travel, just inside
    /// the soft limit).
    pub z_top: f64,
    /// Every joint has a HOME_SEQUENCE, so HOME ALL works.
    pub home_all: bool,
    /// The spindle's top speed, when known.
    pub max_rpm: Option<f64>,
    /// The spindle runs in reverse (M4 is offered).
    pub reverse: bool,
}

impl Default for MachineInfo {
    /// A metric XYZ trivkins machine whose Z top is machine 0.
    fn default() -> Self {
        let axes = (0..3).map(|i| Axis { letter: AXIS_LETTERS[i], index: i, joints: vec![i], rotary: false, home_switch: true }).collect();
        MachineInfo { units: Units::Mm, units_unknown: None, axes, trivial_kins: true, joints: 3, z_top: 0.0, home_all: true, max_rpm: None, reverse: true }
    }
}

impl MachineInfo {
    pub fn from_ini(cfg: &Config) -> Self {
        let raw_units = cfg.get("TRAJ", "LINEAR_UNITS").unwrap_or("mm");
        let (units, units_unknown) = match Units::parse(raw_units) {
            Some(u) => (u, None),
            None => (Units::Mm, Some(raw_units.to_string())),
        };
        let coords = cfg.coordinates().to_ascii_uppercase();
        let kins = cfg.get("KINS", "KINEMATICS").unwrap_or("trivkins");
        let mut words = kins.split_whitespace();
        let trivial_kins = words.next().is_some_and(|k| k == "trivkins");
        // trivkins numbers joints by its coordinates= parameter, or by
        // letter (X 0, Y 1, Z 2, A 3...) without one.
        let kins_coords: Option<String> = kins
            .split_whitespace()
            .find_map(|w| w.strip_prefix("coordinates="))
            .map(|c| c.chars().filter(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase());
        let joints = cfg
            .get("KINS", "JOINTS")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or_else(|| kins_coords.as_ref().map_or(coords.len(), |c| c.len()))
            .min(JOINTS);
        let mut axes: Vec<Axis> = Vec::new();
        for c in coords.chars() {
            let Some(index) = AXIS_LETTERS.iter().position(|a| *a == c) else { continue };
            if axes.iter().any(|a| a.letter == c) {
                continue;
            }
            let joints_of: Vec<usize> = if !trivial_kins {
                Vec::new()
            } else if let Some(kc) = &kins_coords {
                kc.chars().enumerate().filter(|(_, l)| *l == c).map(|(j, _)| j).filter(|j| *j < joints).collect()
            } else {
                (index < joints).then_some(index).into_iter().collect()
            };
            let home_switch = if joints_of.is_empty() {
                true
            } else {
                joints_of.iter().any(|j| cfg.get_f64(&format!("JOINT_{j}"), "HOME_SEARCH_VEL").is_some_and(|v| v != 0.0))
            };
            axes.push(Axis { letter: c, index, joints: joints_of, rotary: matches!(c, 'A' | 'B' | 'C'), home_switch });
        }
        if axes.is_empty() {
            return MachineInfo { units, units_unknown, ..MachineInfo::default() };
        }
        let home_all = (0..joints).all(|j| cfg.get(&format!("JOINT_{j}"), "HOME_SEQUENCE").is_some());
        let z_max = cfg.get_f64("AXIS_Z", "MAX_LIMIT");
        MachineInfo { units, units_unknown, axes, trivial_kins, joints, z_top: z_top(units, z_max), home_all, ..MachineInfo::default() }
    }

    pub fn axis(&self, index: usize) -> Option<&Axis> {
        self.axes.iter().find(|a| a.index == index)
    }

    pub fn axis_by_letter(&self, letter: char) -> Option<&Axis> {
        self.axes.iter().find(|a| a.letter == letter.to_ascii_uppercase())
    }

    /// The first rotary axis, if there is one.
    pub fn rotary(&self) -> Option<&Axis> {
        self.axes.iter().find(|a| a.rotary)
    }

    pub fn has_axis(&self, letter: char) -> bool {
        self.axis_by_letter(letter).is_some()
    }

    /// Every joint behind the axis is homed. Without known joints (not
    /// trivkins), the axis counts as homed once all joints are.
    pub fn axis_homed(&self, s: &Status, index: usize) -> bool {
        match self.axis(index) {
            Some(a) if !a.joints.is_empty() => a.joints.iter().all(|&j| s.homed.get(j).copied().unwrap_or(false)),
            _ => s.all_homed(),
        }
    }

    /// The joint to jog for an axis before homing (free mode jogs joints).
    /// None when the axis has no single joint of its own: a gantry's two
    /// sides must not be jogged apart, and other kinematics are unknown.
    pub fn jog_joint(&self, index: usize) -> Option<usize> {
        match self.axis(index).map(|a| a.joints.as_slice()) {
            Some([j]) => Some(*j),
            _ => None,
        }
    }

    /// The joints to home for one axis; empty when per-axis homing is not
    /// offered (not trivkins).
    pub fn home_joints(&self, letter: char) -> Vec<usize> {
        self.axis_by_letter(letter).map(|a| a.joints.clone()).unwrap_or_default()
    }

    /// Per-axis HOME buttons make sense: every axis has joints of its own.
    pub fn per_axis_home(&self) -> bool {
        self.trivial_kins && self.axes.iter().all(|a| !a.joints.is_empty())
    }

    /// The MDI line that retracts Z to the top of travel. LinuxCNC refuses
    /// G53 in G91, hence the G90.
    pub fn z_top_line(&self) -> String {
        format!("G90 G53 G0 Z{}", self.units.gcode(self.z_top))
    }

    /// A length given in millimetres, in machine units.
    pub fn mm(&self, v: f64) -> f64 {
        self.units.from_mm(v)
    }

    /// Why a screen written for millimetres is off on this machine: PROBE,
    /// JOB and CONV use macros and starting values in mm that have not been
    /// checked in inches yet.
    pub fn units_block(&self, screen: &str) -> Option<String> {
        let name = match screen {
            "probe" => "Probing and tool touch-off",
            "job" => "The job builder",
            "conv" => "Conversational programming",
            _ => return None,
        };
        self.units.inch().then(|| {
            format!(
                "{name} is not available on inch machines yet. Its macros and starting values are written in millimetres \
                 and have not been checked in inches. Everything else (jog, MDI, programs, offsets, tools) works in inches."
            )
        })
    }
}

/// The top of Z travel to retract to: half a millimetre inside
/// [AXIS_Z]MAX_LIMIT, or machine 0 when the limit is within a millimetre of
/// it (the PM-728VT homes Z at the top and calls it 0).
pub fn z_top(units: Units, z_max: Option<f64>) -> f64 {
    let Some(max) = z_max else { return 0.0 };
    if max.abs() <= units.from_mm(1.0) && max >= 0.0 {
        0.0
    } else {
        let v = max - units.from_mm(0.5);
        // No more decimals than the units show.
        let k = 10f64.powi(units.decimals() as i32);
        (v * k).round() / k
    }
}

/// One jog increment from [DISPLAY]INCREMENTS: its label and its size in
/// machine units. `None` is continuous jogging.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub label: String,
    pub size: Option<f64>,
}

/// LinuxCNC's increment list: comma separated (or space separated when
/// there is no comma), each a decimal or a fraction (`1/16`), optionally
/// followed by a unit (`mm`, `cm`, `um`, `in`, `inch`, `mil`); no unit
/// means machine units. `JOG` (Probe Basic's word) is continuous.
/// Continuous always comes first. Entries that cannot be read are returned
/// as problems, never turned into a continuous jog.
pub fn jog_steps(ini: Option<&str>, units: Units) -> (Vec<Step>, Vec<String>) {
    let mut steps = vec![Step { label: "CONT".into(), size: None }];
    let mut bad = Vec::new();
    let Some(text) = ini.map(str::trim).filter(|t| !t.is_empty()) else {
        let defaults: &[f64] = if units.inch() { &[0.0001, 0.001, 0.01, 0.1] } else { &[0.01, 0.1, 1.0, 10.0] };
        steps.extend(defaults.iter().map(|&v| Step { label: trim_num(v), size: Some(v) }));
        return (steps, bad);
    };
    let items: Vec<&str> = if text.contains(',') { text.split(',').collect() } else { text.split_whitespace().collect() };
    for item in items.into_iter().map(str::trim).filter(|s| !s.is_empty()) {
        if matches!(item.to_ascii_uppercase().as_str(), "JOG" | "CONT" | "CONTINUOUS") {
            continue;
        }
        match parse_step(item, units) {
            Some(s) if !steps.iter().any(|o| o.size == s.size) => steps.push(s),
            Some(_) => {}
            None => bad.push(item.to_string()),
        }
    }
    (steps, bad)
}

fn trim_num(v: f64) -> String {
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn parse_step(item: &str, units: Units) -> Option<Step> {
    let lower = item.to_ascii_lowercase();
    let num_end = lower.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '/' || c == ' ')).unwrap_or(lower.len());
    let (num, unit) = lower.split_at(num_end);
    let num: String = num.chars().filter(|c| !c.is_whitespace()).collect();
    let value = match num.split_once('/') {
        Some((a, b)) => {
            let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
            (b != 0.0).then(|| a / b)?
        }
        None => num.parse::<f64>().ok()?,
    };
    // Size of one of the written unit, in mm; None is machine units.
    let unit_mm = match unit.trim() {
        "" => None,
        "mm" => Some(1.0),
        "cm" => Some(10.0),
        "um" => Some(0.001),
        "in" | "inch" => Some(25.4),
        "mil" => Some(0.0254),
        _ => return None,
    };
    let size = match unit_mm {
        None => value,
        Some(mm) => units.from_mm(value * mm),
    };
    if !(size > 0.0) || !size.is_finite() {
        return None;
    }
    // The label as written, without the unit when it is the machine's.
    let same = unit_mm.is_none() || (units.inch() && unit_mm == Some(25.4)) || (!units.inch() && unit_mm == Some(1.0));
    let label = if same { if num.contains('/') { num } else { trim_num(value) } } else { format!("{}{}", if num.contains('/') { num } else { trim_num(value) }, unit.trim()) };
    Some(Step { label, size: Some(size) })
}

static INFO: OnceLock<MachineInfo> = OnceLock::new();

/// Read the machine from the INI; the first call wins.
pub fn init(cfg: &Config, mc: &crate::machine_cfg::MachineCfg) -> &'static MachineInfo {
    INFO.get_or_init(|| {
        let mut mi = MachineInfo::from_ini(cfg);
        if let Some(z) = mc.machine.z_top {
            mi.z_top = z;
        }
        mi.max_rpm = mc.max_rpm(cfg);
        mi.reverse = mc.reverse(cfg);
        mi
    })
}

/// The machine, or the default metric XYZ machine before `init` (tests).
pub fn get() -> &'static MachineInfo {
    static DEFAULT: OnceLock<MachineInfo> = OnceLock::new();
    INFO.get().unwrap_or_else(|| DEFAULT.get_or_init(MachineInfo::default))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ini(text: &str) -> Config {
        let d = std::env::temp_dir().join(format!("millgui-machine-{}-{}", std::process::id(), text.len()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("m.ini"), text).unwrap();
        let c = Config::load(&d.join("m.ini")).unwrap();
        std::fs::remove_dir_all(d).ok();
        c
    }

    #[test]
    fn units_from_linear_units() {
        assert_eq!(Units::parse("mm "), Some(Units::Mm));
        assert_eq!(Units::parse("metric"), Some(Units::Mm));
        assert_eq!(Units::parse("inch"), Some(Units::Inch));
        assert_eq!(Units::parse("imperial"), Some(Units::Inch));
        assert_eq!(Units::parse("1.0"), Some(Units::Mm));
        assert_eq!(Units::parse("0.03937007874"), Some(Units::Inch));
        assert_eq!(Units::parse("furlong"), None);
        assert_eq!(Units::Inch.gcode(0.1003), "0.1003");
        assert_eq!(Units::Mm.gcode(-0.0), "0");
        assert_eq!(Units::Mm.gcode(99.5), "99.5");
    }

    #[test]
    fn mymill_is_metric_xyza_with_z_top_at_zero() {
        let c = ini("[TRAJ]\nCOORDINATES = XYZA\nLINEAR_UNITS = mm \n[KINS]\nKINEMATICS = trivkins coordinates=XYZA\nJOINTS = 4\n\
            [AXIS_Z]\nMAX_LIMIT = 0.001\n[JOINT_0]\nHOME_SEARCH_VEL = -50\nHOME_SEQUENCE = 3\n[JOINT_1]\nHOME_SEARCH_VEL = 50\nHOME_SEQUENCE = 2\n\
            [JOINT_2]\nHOME_SEARCH_VEL = 25\nHOME_SEQUENCE = 1\n[JOINT_3]\nHOME_SEARCH_VEL = 0\nHOME_SEQUENCE = 0\n");
        let m = MachineInfo::from_ini(&c);
        assert_eq!(m.units, Units::Mm);
        assert_eq!(m.z_top, 0.0);
        assert_eq!(m.z_top_line(), "G90 G53 G0 Z0");
        assert!(m.home_all && m.per_axis_home());
        let a = m.rotary().unwrap();
        assert_eq!((a.letter, a.index, a.joints.as_slice(), a.home_switch), ('A', 3, &[3][..], false));
        assert!(m.axis(2).unwrap().home_switch);
    }

    #[test]
    fn stock_inch_sim() {
        // LinuxCNC's configs/sim/axis/axis.ini
        let c = ini("[TRAJ]\nCOORDINATES = X Y Z\nLINEAR_UNITS = inch\n[KINS]\nKINEMATICS = trivkins\nJOINTS = 3\n[AXIS_Z]\nMAX_LIMIT = 0.12\n");
        let m = MachineInfo::from_ini(&c);
        assert_eq!(m.units, Units::Inch);
        assert!((m.z_top - 0.1003).abs() < 1e-9);
        assert_eq!(m.z_top_line(), "G90 G53 G0 Z0.1003");
        assert_eq!(m.axes.iter().map(|a| a.joints.clone()).collect::<Vec<_>>(), [vec![0], vec![1], vec![2]]);
        assert!(!m.home_all);
    }

    #[test]
    fn gantry_and_letter_order() {
        let c = ini("[TRAJ]\nCOORDINATES = XYYZ\n[KINS]\nKINEMATICS = trivkins coordinates=XYYZ kinstype=BOTH\nJOINTS = 4\n");
        let m = MachineInfo::from_ini(&c);
        assert_eq!(m.axis_by_letter('Y').unwrap().joints, [1, 2]);
        assert_eq!(m.axis_by_letter('Z').unwrap().joints, [3]);
        assert_eq!(m.jog_joint(1), None);
        assert_eq!(m.jog_joint(2), Some(3));
        assert_eq!(m.home_joints('Y'), [1, 2]);
        // XYZB with coordinates=: B is joint 3, not 4.
        let c = ini("[TRAJ]\nCOORDINATES = XYZB\n[KINS]\nKINEMATICS = trivkins coordinates=XYZB\nJOINTS = 4\n");
        let m = MachineInfo::from_ini(&c);
        assert_eq!(m.rotary().unwrap().joints, [3]);
        assert_eq!(m.jog_joint(4), Some(3));
        // Not trivkins: nothing per axis.
        let c = ini("[TRAJ]\nCOORDINATES = XYZ\n[KINS]\nKINEMATICS = corexykins\nJOINTS = 3\n");
        let m = MachineInfo::from_ini(&c);
        assert!(!m.per_axis_home());
        assert_eq!(m.jog_joint(0), None);
    }

    #[test]
    fn inch_machines_block_probe_job_and_conv() {
        let inch = MachineInfo { units: Units::Inch, ..MachineInfo::default() };
        for screen in ["probe", "job", "conv"] {
            assert!(inch.units_block(screen).is_some());
            assert!(MachineInfo::default().units_block(screen).is_none());
        }
        assert!(inch.units_block("manual").is_none());
    }

    #[test]
    fn z_top_rules() {
        assert_eq!(z_top(Units::Mm, Some(0.001)), 0.0);
        assert_eq!(z_top(Units::Mm, Some(0.0)), 0.0);
        assert_eq!(z_top(Units::Mm, Some(100.0)), 99.5);
        assert_eq!(z_top(Units::Mm, Some(-10.0)), -10.5);
        assert_eq!(z_top(Units::Mm, None), 0.0);
        assert_eq!(z_top(Units::Inch, Some(4.0)), 3.9803);
    }

    #[test]
    fn jog_increments() {
        let labels = |s: &[Step]| s.iter().map(|s| s.label.clone()).collect::<Vec<_>>();
        let (s, bad) = jog_steps(Some("JOG,0.01,0.1,1,10 "), Units::Mm);
        assert_eq!(labels(&s), ["CONT", "0.01", "0.1", "1", "10"]);
        assert_eq!(s[3].size, Some(1.0));
        assert!(bad.is_empty());
        // LinuxCNC's sim axis.ini, on its inch machine.
        let (s, bad) = jog_steps(Some("1 in, 0.1 in, 10 mil, 1 mil, 1mm, .1mm, 1/8000 in"), Units::Inch);
        assert_eq!(labels(&s), ["CONT", "1", "0.1", "10mil", "1mil", "1mm", "0.1mm", "1/8000"]);
        assert_eq!(s[3].size, Some(0.01));
        assert!((s[5].size.unwrap() - 1.0 / 25.4).abs() < 1e-12);
        assert_eq!(s[7].size, Some(1.0 / 8000.0));
        assert!(bad.is_empty());
        // Space separated, units glued on.
        let (s, _) = jog_steps(Some(".1in .05in .01in .001in"), Units::Inch);
        assert_eq!(labels(&s), ["CONT", "0.1", "0.05", "0.01", "0.001"]);
        // On a metric machine, inch steps convert.
        let (s, _) = jog_steps(Some("1/16 in, 1 mm"), Units::Mm);
        assert_eq!(labels(&s), ["CONT", "1/16in", "1"]);
        assert_eq!(s[1].size, Some(25.4 / 16.0));
        // Unreadable entries are dropped and reported, never continuous.
        let (s, bad) = jog_steps(Some("0.1, fast, 2 furlong, 0"), Units::Mm);
        assert_eq!(labels(&s), ["CONT", "0.1"]);
        assert_eq!(bad, ["fast", "2 furlong", "0"]);
        // None set: defaults in machine units.
        assert_eq!(labels(&jog_steps(None, Units::Inch).0), ["CONT", "0.0001", "0.001", "0.01", "0.1"]);
    }
}
