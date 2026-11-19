//! The probe's and tool setter's stored values, #3004 to #3037.
//!
//! The probe macros read these parameters. ferrum owns them: each one comes
//! from ferrum.toml (`[probe.defaults]`, `[toolsetter]`), else from the
//! parameter file (machines set up with Probe Basic have them there), else a
//! starting value for a 2 mm tip touch probe. Values with none of the three
//! (the setter's max Z travel and reference height) must be set by the user.
//! The probe module writes them to LinuxCNC; edits are saved to ferrum.toml.

use crate::config::Config;
use crate::machine_cfg::MachineCfg;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Feed,
    Dist,
}

pub struct Def {
    pub n: u16,
    /// The ferrum.toml key, `table.key` or `probe.defaults.key`.
    pub key: &'static str,
    pub title: &'static str,
    pub unit: Unit,
    pub seed_mm: Option<f64>,
    pub seed_inch: Option<f64>,
    get: fn(&MachineCfg) -> Option<f64>,
}

impl Def {
    pub fn toml(&self, mc: &MachineCfg) -> Option<f64> {
        (self.get)(mc)
    }
    pub fn seed(&self, inch: bool) -> Option<f64> {
        if inch { self.seed_inch } else { self.seed_mm }
    }
    pub fn setter(&self) -> bool {
        self.key.starts_with("toolsetter.")
    }
}

const fn d(n: u16, key: &'static str, title: &'static str, unit: Unit, mm: Option<f64>, inch: Option<f64>, get: fn(&MachineCfg) -> Option<f64>) -> Def {
    Def { n, key, title, unit, seed_mm: mm, seed_inch: inch, get }
}

use Unit::{Dist, Feed};

/// The values ferrum owns, in SETUP's order. The hints (#3025 to #3029)
/// and modes (#3011 to #3013, #3030, #3031, #3036) are per run.
pub const DEFS: &[Def] = &[
    d(3015, "probe.defaults.slow_feed", "Slow feed (the touch)", Feed, Some(25.0), Some(1.0), |c| c.probe.defaults.slow_feed),
    d(3016, "probe.defaults.fast_feed", "Fast feed (finding)", Feed, Some(300.0), Some(12.0), |c| c.probe.defaults.fast_feed),
    d(3017, "probe.defaults.traverse_feed", "Traverse feed", Feed, Some(1500.0), Some(60.0), |c| c.probe.defaults.traverse_feed),
    d(3018, "probe.defaults.max_xy", "Max XY search distance", Dist, Some(15.0), Some(0.6), |c| c.probe.defaults.max_xy),
    d(3019, "probe.defaults.xy_clearance", "XY clearance", Dist, Some(5.0), Some(0.2), |c| c.probe.defaults.xy_clearance),
    d(3020, "probe.defaults.max_z", "Max Z search distance", Dist, Some(15.0), Some(0.6), |c| c.probe.defaults.max_z),
    d(3021, "probe.defaults.z_clearance", "Z clearance", Dist, Some(5.0), Some(0.2), |c| c.probe.defaults.z_clearance),
    d(3022, "probe.defaults.extra_depth", "Extra probe depth", Dist, Some(0.0), Some(0.0), |c| c.probe.defaults.extra_depth),
    d(3023, "probe.defaults.step_off", "Step off width", Dist, Some(10.0), Some(0.4), |c| c.probe.defaults.step_off),
    d(3024, "probe.defaults.edge_width", "Edge width", Dist, Some(20.0), Some(0.8), |c| c.probe.defaults.edge_width),
    d(3032, "probe.defaults.calibration_offset", "Calibration offset (from CAL)", Dist, Some(0.0), Some(0.0), |c| c.probe.defaults.calibration_offset),
    d(3033, "probe.defaults.ring_gauge", "Ring gauge diameter", Dist, Some(25.0), Some(1.0), |c| c.probe.defaults.ring_gauge),
    d(3034, "probe.defaults.pin_gauge", "Gauge pin diameter", Dist, Some(10.0), Some(0.375), |c| c.probe.defaults.pin_gauge.or(c.probe.defaults.cal_block_x)),
    d(3035, "probe.defaults.cal_block_y", "Calibration block Y width", Dist, None, None, |c| c.probe.defaults.cal_block_y),
    d(3004, "toolsetter.fast_feed", "Fast feed (finding)", Feed, Some(300.0), Some(12.0), |c| c.toolsetter.fast_feed),
    d(3005, "toolsetter.slow_feed", "Slow feed (the touch)", Feed, Some(25.0), Some(1.0), |c| c.toolsetter.slow_feed),
    d(3006, "toolsetter.traverse_feed", "Traverse feed", Feed, Some(1500.0), Some(60.0), |c| c.toolsetter.traverse_feed),
    d(3007, "toolsetter.max_z", "Max Z travel down from G30", Dist, None, None, |c| c.toolsetter.max_z),
    d(3009, "toolsetter.retract", "Retract after the first touch", Dist, Some(2.0), Some(0.08), |c| c.toolsetter.retract),
    d(3010, "toolsetter.reference_height", "Reference height (spindle zero)", Dist, None, None, |c| c.toolsetter.reference_height),
    d(3037, "toolsetter.breakage_tolerance", "Breakage tolerance", Dist, Some(0.5), Some(0.02), |c| c.toolsetter.breakage_tolerance),
];

/// The tool check's tolerance (`#3037`). Probe Basic left it 0 (unused), so
/// a 0 in the parameter file means "not set" and the starting value applies.
pub const BREAKAGE_TOLERANCE: u16 = 3037;

/// The probe tool's number, for the macros (`#3014`); from `[probe]tool`.
pub const PROBE_TOOL: u16 = 3014;

pub fn def(n: u16) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.n == n)
}

pub fn by_key(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Toml,
    ParamFile,
    /// A starting value: the user has not confirmed it.
    Seed,
    Missing,
}

/// #3004 to #3037 as the parameter file has them.
pub fn param_file(cfg: &Config) -> BTreeMap<u16, f64> {
    let path = cfg.dir().join(cfg.get("RS274NGC", "PARAMETER_FILE").unwrap_or("linuxcnc.var"));
    std::fs::read_to_string(path).map(|t| parse_params(&t)).unwrap_or_default()
}

pub fn parse_params(text: &str) -> BTreeMap<u16, f64> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let mut w = line.split_whitespace();
        if let (Some(Ok(n)), Some(Ok(v))) = (w.next().map(str::parse::<u16>), w.next().map(str::parse::<f64>)) {
            if (3004..=3037).contains(&n) {
                out.insert(n, v);
            }
        }
    }
    out
}

/// The value ferrum uses for each owned parameter, and where it came from.
/// `base` is what LinuxCNC holds (the parameter file, or the saved copy of
/// an earlier run's defaults).
pub fn resolve(mc: &MachineCfg, base: &BTreeMap<u16, f64>, inch: bool) -> BTreeMap<u16, (Option<f64>, Source)> {
    DEFS.iter()
        .map(|d| {
            let r = if let Some(v) = d.toml(mc) {
                (Some(v), Source::Toml)
            } else if let Some(v) = base.get(&d.n).filter(|v| !(d.n == BREAKAGE_TOLERANCE && **v <= 0.0)) {
                (Some(*v), Source::ParamFile)
            } else if let Some(v) = d.seed(inch) {
                (Some(v), Source::Seed)
            } else {
                (None, Source::Missing)
            };
            (d.n, r)
        })
        .collect()
}

/// Checker rows' words: the titles of the values in `which` (probe or
/// setter) that come from `src`.
pub fn titles(r: &BTreeMap<u16, (Option<f64>, Source)>, setter: bool, src: Source) -> Vec<&'static str> {
    DEFS.iter().filter(|d| d.setter() == setter && r.get(&d.n).is_some_and(|(_, s)| *s == src)).map(|d| d.title).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_beats_the_parameter_file_beats_the_seed() {
        let (mc, p) = MachineCfg::parse("[probe.defaults]\nslow_feed = 40\n[toolsetter]\nmax_z = 120.5\n");
        assert!(p.is_empty(), "{p:?}");
        let base = parse_params("3015\t80.0\n3016\t500.000000\n3099 1\n");
        let r = resolve(&mc, &base, false);
        assert_eq!(r[&3015], (Some(40.0), Source::Toml));
        assert_eq!(r[&3016], (Some(500.0), Source::ParamFile));
        assert_eq!(r[&3017], (Some(1500.0), Source::Seed));
        assert_eq!(r[&3007], (Some(120.5), Source::Toml));
        assert_eq!(r[&3010], (None, Source::Missing));
        assert_eq!(resolve(&mc, &base, true)[&3017], (Some(60.0), Source::Seed));
        assert_eq!(titles(&r, true, Source::Missing), ["Reference height (spindle zero)"]);
    }

    #[test]
    fn every_key_is_read_from_ferrum_toml() {
        for d in DEFS {
            let (table, key) = d.key.rsplit_once('.').unwrap();
            let text = format!("[{table}]\n{key} = 1.25\n");
            let (mc, p) = MachineCfg::parse(&text);
            assert!(p.is_empty(), "{}: {p:?}", d.key);
            assert_eq!(d.toml(&mc), Some(1.25), "{}", d.key);
        }
        let mut ns: Vec<u16> = DEFS.iter().map(|d| d.n).collect();
        ns.sort();
        ns.dedup();
        assert_eq!(ns.len(), DEFS.len());
    }

    #[test]
    fn a_misspelt_key_is_reported() {
        let (_, p) = MachineCfg::parse("[probe.defaults]\nslowfeed = 40\n");
        assert_eq!(p.len(), 1, "{p:?}");
    }
}
