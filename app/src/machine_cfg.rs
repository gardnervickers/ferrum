//! ferrum.toml: ferrum's own settings for one machine, beside the INI
//! (`[FERRUM]CONFIG` moves it). The INI keeps LinuxCNC's machine facts;
//! this file holds what LinuxCNC does not know: which modules, how M6 is
//! answered, the probe, the HAL signal names, the backplot table.
//!
//! Every key is optional. A missing key means "detect" or the default; a
//! table with a bad key is reported and that table alone falls back to its
//! defaults. With no ferrum.toml, `machine.toml` (`[FERRUM]MACHINE_CONFIG`,
//! else `ferrum/machine.toml`) is still read for `[buttons]` and `[model]`,
//! and the INI's `[FERRUM]` keys (MODULES, POSTGUI_HALFILE, REMOTE_PORT,
//! REMOTE_ROOT, RENDERER, MDI_LIBRARY) are honoured; ferrum.toml wins.

use crate::config::Config;
use crate::{Live, MainWindow};
use preview::machine::Machine;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use slint::ComponentHandle;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(test)]
const BUILTIN: &str = include_str!("../../machine.toml");

/// The file name looked for beside the INI.
pub const FILE: &str = "ferrum.toml";
/// The settings file's name before the rename; read when there is no ferrum.toml.
pub const LEGACY_FILE: &str = "millgui.toml";

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Buttons {
    pub mist: bool,
    pub flood: bool,
    pub drawbar: bool,
    pub home_all: bool,
}

impl Default for Buttons {
    fn default() -> Self {
        Buttons { mist: true, flood: true, drawbar: true, home_all: true }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Model {
    pub show: bool,
    pub table: [f64; 3],
    pub table_center: [f64; 2],
    pub slots: u32,
    pub nose_above_table: f64,
}

impl Default for Model {
    fn default() -> Self {
        let m = Machine::pm728vt();
        Model { show: false, table: m.table, table_center: m.table_center, slots: m.slots, nose_above_table: 0.0 }
    }
}

impl Model {
    /// The table is described: the spindle nose height above it is set.
    pub fn known(&self) -> bool {
        self.nose_above_table > 0.0
    }

    pub fn machine(&self) -> Machine {
        Machine {
            table: self.table,
            table_center: self.table_center,
            slots: self.slots,
            nose_above_table: self.nose_above_table,
        }
    }
}

/// Where PARK goes after Z is at the top.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Park {
    /// Machine X0 Y0 (and the rotary to 0).
    #[default]
    MachineZero,
    /// LinuxCNC's G28 position.
    G28,
    /// Machine X Y.
    Xy(f64, f64),
}

impl<'de> Deserialize<'de> for Park {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Name(String),
            Xy([f64; 2]),
        }
        match Raw::deserialize(d)? {
            Raw::Xy([x, y]) => Ok(Park::Xy(x, y)),
            Raw::Name(n) => match n.as_str() {
                "machine-zero" => Ok(Park::MachineZero),
                "g28" => Ok(Park::G28),
                _ => Err(serde::de::Error::custom(format!("park = \"{n}\": use \"machine-zero\", \"g28\" or [x, y]"))),
            },
        }
    }
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MachineSec {
    /// Machine Z to retract to; default from `[AXIS_Z]MAX_LIMIT`.
    pub z_top: Option<f64>,
    pub park: Park,
}

/// A list of words, written as a TOML list or one string.
#[derive(Clone, Debug, PartialEq)]
pub struct Words(pub Vec<String>);

impl<'de> Deserialize<'de> for Words {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            List(Vec<String>),
            Text(String),
        }
        Ok(Words(match Raw::deserialize(d)? {
            Raw::List(v) => v,
            Raw::Text(t) => split_words(&t),
        }))
    }
}

pub fn split_words(t: &str) -> Vec<String> {
    t.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).map(String::from).collect()
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ModulesSec {
    /// In nav order; unset: `[FERRUM]MODULES`, else the default list.
    pub enabled: Option<Words>,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SpindleSec {
    /// Unset: `[SPINDLE_0]MAX_FORWARD_VELOCITY`.
    pub max_rpm: Option<f64>,
    /// Unset: false when `[SPINDLE_0]MAX_REVERSE_VELOCITY = 0`.
    pub reverse: Option<bool>,
}

/// Who answers M6.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolChangeStyle {
    /// Something in the machine's HAL (an ATC, hal_manualtoolchange, a
    /// remap) answers; ferrum shows the tool number.
    External,
    /// ferrum's tool change overlay answers.
    Dialog,
    /// The overlay plus a power drawbar and touch-off latch (mymill).
    Drawbar,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ToolChangeSec {
    /// Unset: detected.
    pub style: Option<ToolChangeStyle>,
    /// Tools in this range (inclusive) are scratch tools: a change to one
    /// warns that a touch-off is likely needed.
    pub scratch_tools: Option<[u32; 2]>,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ProbeSec {
    pub enabled: Option<bool>,
    /// Unset: `[PROBE]TOOL_NUMBER`, else 99.
    pub tool: Option<u32>,
    /// #3015 to #3035 (probe_defaults.rs). Unset: the parameter file's
    /// value, else a starting value.
    pub defaults: ProbeDefaults,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ProbeDefaults {
    pub slow_feed: Option<f64>,
    pub fast_feed: Option<f64>,
    pub traverse_feed: Option<f64>,
    pub max_xy: Option<f64>,
    pub xy_clearance: Option<f64>,
    pub max_z: Option<f64>,
    pub z_clearance: Option<f64>,
    pub extra_depth: Option<f64>,
    pub step_off: Option<f64>,
    pub edge_width: Option<f64>,
    pub calibration_offset: Option<f64>,
    pub ring_gauge: Option<f64>,
    /// #3034: the gauge pin CAL › PROBE TIP › PIN measures. Older files
    /// call it `cal_block_x` (read when `pin_gauge` is missing).
    pub pin_gauge: Option<f64>,
    pub cal_block_x: Option<f64>,
    pub cal_block_y: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ToolSetterSec {
    pub enabled: Option<bool>,
    /// #3004 to #3010 and #3037 (probe_defaults.rs).
    pub fast_feed: Option<f64>,
    pub slow_feed: Option<f64>,
    pub traverse_feed: Option<f64>,
    pub max_z: Option<f64>,
    pub retract: Option<f64>,
    pub reference_height: Option<f64>,
    pub breakage_tolerance: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteSec {
    pub port: Option<u16>,
    pub root: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DisplaySec {
    pub renderer: Option<String>,
    pub mdi_library: Option<String>,
    /// HAL files run after ferrum's pins exist (relative to the INI).
    pub postgui: Option<Words>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MachineCfg {
    /// The file read, if any.
    pub path: Option<PathBuf>,
    /// The file as written, to tell a key that is set from a default.
    pub raw: toml::Table,
    pub machine: MachineSec,
    pub modules: ModulesSec,
    pub spindle: SpindleSec,
    pub toolchange: ToolChangeSec,
    pub probe: ProbeSec,
    pub toolsetter: ToolSetterSec,
    /// Logical name -> HAL signal or pin.
    pub hal: BTreeMap<String, String>,
    pub remote: RemoteSec,
    pub display: DisplaySec,
    pub buttons: Buttons,
    pub model: Model,
}

fn section<T: DeserializeOwned + Default>(t: &toml::Table, name: &str, problems: &mut Vec<String>) -> T {
    match t.get(name) {
        None => T::default(),
        Some(v) => v.clone().try_into().unwrap_or_else(|e: toml::de::Error| {
            problems.push(format!("[{name}]: {}; using the defaults for [{name}]", e.message().trim()));
            T::default()
        }),
    }
}

const SECTIONS: &[&str] =
    &["machine", "modules", "spindle", "toolchange", "probe", "toolsetter", "hal", "remote", "display", "buttons", "model"];

impl MachineCfg {
    /// Parse a file's text. A table with a bad key or value falls back to
    /// its defaults alone; each problem is returned.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        let t: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => {
                let e: toml::de::Error = e;
                problems.push(format!("{}; using the defaults", e.to_string().trim()));
                return (Self::default(), problems);
            }
        };
        for k in t.keys() {
            if !SECTIONS.contains(&k.as_str()) {
                problems.push(format!("unknown table [{k}] (ignored)"));
            }
        }
        let c = MachineCfg {
            path: None,
            raw: t.clone(),
            machine: section(&t, "machine", &mut problems),
            modules: section(&t, "modules", &mut problems),
            spindle: section(&t, "spindle", &mut problems),
            toolchange: section(&t, "toolchange", &mut problems),
            probe: section(&t, "probe", &mut problems),
            toolsetter: section(&t, "toolsetter", &mut problems),
            hal: section(&t, "hal", &mut problems),
            remote: section(&t, "remote", &mut problems),
            display: section(&t, "display", &mut problems),
            buttons: section(&t, "buttons", &mut problems),
            model: section(&t, "model", &mut problems),
        };
        (c, problems)
    }

    /// ferrum.toml beside the INI (or `[FERRUM]CONFIG`); without one, the
    /// older machine.toml. Problems go to `notes`, prefixed with the file.
    pub fn load(cfg: &Config, notes: &mut Vec<String>) -> Self {
        let named = cfg.get("FERRUM", "CONFIG").map(|p| cfg.dir().join(p.trim()));
        let path = named.clone().unwrap_or_else(|| cfg.dir().join(FILE));
        if let Some(c) = Self::read(&path, notes) {
            return c;
        }
        if named.is_none() {
            if let Some(c) = Self::read(&cfg.dir().join(LEGACY_FILE), notes) {
                return c;
            }
        }
        if named.is_some() {
            notes.push(format!("{}: [FERRUM]CONFIG names a file that is not there", path.display()));
        }
        let old = cfg.get("FERRUM", "MACHINE_CONFIG").map(|p| cfg.dir().join(p)).unwrap_or_else(|| cfg.dir().join("ferrum/machine.toml"));
        Self::read(&old, notes).unwrap_or_default()
    }

    fn read(path: &Path, notes: &mut Vec<String>) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let (mut c, problems) = Self::parse(&text);
        notes.extend(problems.into_iter().map(|p| format!("{}: {p}", path.display())));
        c.path = Some(path.to_path_buf());
        Some(c)
    }

    /// The ferrum.toml SETUP writes (the one read, or the one ferrum
    /// would read), and the older file to start a new one from.
    pub fn target(&self, cfg: &Config) -> (PathBuf, Option<PathBuf>) {
        let path = cfg.get("FERRUM", "CONFIG").map(|p| cfg.dir().join(p.trim())).unwrap_or_else(|| {
            // A machine set up before the rename keeps its millgui.toml.
            let legacy = cfg.dir().join(LEGACY_FILE);
            if !cfg.dir().join(FILE).exists() && legacy.exists() { legacy } else { cfg.dir().join(FILE) }
        });
        let seed = self.path.clone().filter(|p| *p != path);
        (path, seed)
    }

    pub fn apply(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        live.set_quick_mist(self.buttons.mist);
        live.set_quick_flood(self.buttons.flood);
        live.set_quick_drawbar(self.buttons.drawbar);
        live.set_quick_home(self.buttons.home_all);
        live.set_has_scratch(self.toolchange.scratch_tools.is_some());
    }

    /// `[table]key` is written in the file.
    pub fn is_set(&self, table: &str, key: &str) -> bool {
        self.raw.get(table).and_then(|t| t.as_table()).is_some_and(|t| t.contains_key(key))
    }

    /// The modules, in nav order: `[modules]enabled`, else `[FERRUM]MODULES`,
    /// else `default`.
    pub fn module_ids(&self, cfg: &Config, default: &str) -> Vec<String> {
        match &self.modules.enabled {
            Some(w) => w.0.clone(),
            None => split_words(cfg.get("FERRUM", "MODULES").unwrap_or(default)),
        }
    }

    /// HAL files to run after the modules' own (relative to the INI).
    pub fn postgui(&self, cfg: &Config) -> Vec<String> {
        match &self.display.postgui {
            Some(w) => w.0.clone(),
            None => cfg.get("FERRUM", "POSTGUI_HALFILE").map(|s| vec![s.trim().to_string()]).unwrap_or_default(),
        }
    }

    /// The remote screen's port: None when off, Err when the INI's is not a number.
    pub fn remote_port(&self, cfg: &Config) -> Option<Result<u16, String>> {
        if let Some(p) = self.remote.port {
            // 0: off, even when the INI names a port.
            return (p != 0).then_some(Ok(p));
        }
        let p = cfg.get("FERRUM", "REMOTE_PORT")?;
        Some(p.trim().parse::<u16>().map_err(|_| "[FERRUM]REMOTE_PORT is not a port number".to_string()))
    }

    pub fn remote_root(&self, cfg: &Config) -> Option<String> {
        self.remote.root.clone().or_else(|| cfg.get("FERRUM", "REMOTE_ROOT").map(String::from))
    }

    pub fn renderer(&self, cfg: &Config) -> Option<String> {
        self.display.renderer.clone().or_else(|| cfg.get("FERRUM", "RENDERER").map(String::from))
    }

    pub fn mdi_library(&self, cfg: &Config) -> Option<String> {
        self.display.mdi_library.clone().or_else(|| cfg.get("FERRUM", "MDI_LIBRARY").map(String::from))
    }

    /// The probe's tool number: `[probe]tool`, else `[PROBE]TOOL_NUMBER`, else 99.
    pub fn probe_tool(&self, cfg: &Config) -> i32 {
        self.probe
            .tool
            .map(|t| t as i32)
            .or_else(|| cfg.get("PROBE", "TOOL_NUMBER").and_then(|v| v.trim().parse().ok()))
            .unwrap_or(99)
    }

    /// The spindle's top speed, if known.
    pub fn max_rpm(&self, cfg: &Config) -> Option<f64> {
        self.spindle.max_rpm.or_else(|| cfg.get_f64("SPINDLE_0", "MAX_FORWARD_VELOCITY")).filter(|v| *v > 0.0)
    }

    /// The spindle runs in reverse (M4).
    pub fn reverse(&self, cfg: &Config) -> bool {
        self.spindle.reverse.unwrap_or_else(|| cfg.get_f64("SPINDLE_0", "MAX_REVERSE_VELOCITY") != Some(0.0))
    }

    /// The scratch tool numbers, if set.
    pub fn scratch_range(&self) -> Option<std::ops::RangeInclusive<i32>> {
        self.toolchange.scratch_tools.map(|[lo, hi]| lo as i32..=hi as i32)
    }

    /// A change to this tool warns that a touch-off is likely needed.
    pub fn is_scratch_tool(&self, tool: i32) -> bool {
        self.toolchange.scratch_tools.is_some_and(|[lo, hi]| tool >= lo as i32 && tool <= hi as i32)
    }
}

/// `text` with `key` (`table.key`, or `table.sub.key`) set to `value`, or
/// removed for None. Comments and layout are kept. Refused when the result
/// would not read cleanly.
pub fn edited(text: &str, key: &str, value: Option<toml_edit::Item>) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| format!("ferrum.toml does not read: {}", e.message()))?;
    let (tables, k) = key.rsplit_once('.').ok_or("bad key")?;
    let top = tables.split('.').next().unwrap_or(tables);
    match value {
        Some(v) => {
            let mut t: &mut dyn toml_edit::TableLike = doc.as_table_mut();
            for (i, name) in tables.split('.').enumerate() {
                let nested = i + 1 < tables.split('.').count();
                let e = t.entry(name).or_insert_with(|| {
                    let mut n = toml_edit::Table::new();
                    // [probe.defaults] alone, not an empty [probe] above it.
                    n.set_implicit(nested);
                    toml_edit::Item::Table(n)
                });
                t = e.as_table_like_mut().ok_or_else(|| format!("[{tables}] in ferrum.toml is not a table"))?;
            }
            t.insert(k, v);
        }
        None => {
            let mut t: Option<&mut dyn toml_edit::TableLike> = Some(doc.as_table_mut());
            for name in tables.split('.') {
                t = t.and_then(|t| t.get_mut(name)).and_then(|i| i.as_table_like_mut());
            }
            if let Some(t) = t {
                t.remove(k);
            }
        }
    }
    let out = doc.to_string();
    let (_, problems) = MachineCfg::parse(&out);
    if let Some(p) = problems.iter().find(|p| p.starts_with(&format!("[{top}]"))) {
        return Err(p.clone());
    }
    Ok(out)
}

pub const HEADER: &str = "# ferrum's settings for this machine (ferrum/docs/ferrum-toml.md).\n# SETUP writes what it changes here; every key is optional.\n";

/// A new ferrum.toml: the older machine.toml's settings, under a header.
pub fn new_file_text(seed: Option<&Path>) -> String {
    let old = seed.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    if old.is_empty() { HEADER.to_string() } else { format!("{HEADER}\n{old}") }
}

/// Write one key to the ferrum.toml at `path` (started from `seed`, the
/// older machine.toml, when there is none yet).
pub fn write_key(path: &Path, seed: Option<&Path>, key: &str, value: Option<toml_edit::Item>) -> Result<(), String> {
    let (text, new) = match std::fs::read_to_string(path) {
        Ok(t) => (t, false),
        Err(_) => (seed.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default(), true),
    };
    let out = edited(&text, key, value)?;
    let out = if new { format!("{HEADER}\n{out}") } else { out };
    write_file(path, &out)
}

/// Write through a temporary file, so a crash never leaves half a file.
pub fn write_file(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

static CFG: OnceLock<MachineCfg> = OnceLock::new();

/// Load ferrum.toml; the first call wins.
pub fn init(cfg: &Config, notes: &mut Vec<String>) -> &'static MachineCfg {
    if let Some(c) = CFG.get() {
        return c;
    }
    let c = MachineCfg::load(cfg, notes);
    CFG.get_or_init(|| c)
}

/// The settings, or the defaults before `init` (tests).
pub fn get() -> &'static MachineCfg {
    static DEFAULT: OnceLock<MachineCfg> = OnceLock::new();
    CFG.get().unwrap_or_else(|| DEFAULT.get_or_init(MachineCfg::default))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(t: &str) -> MachineCfg {
        let (c, p) = MachineCfg::parse(t);
        assert!(p.is_empty(), "{p:?}");
        c
    }

    #[test]
    fn shipped_file_is_the_pm728vt() {
        let c = parse(BUILTIN);
        let m = Machine::pm728vt();
        assert!(c.model.show && c.model.known());
        assert_eq!((c.model.table, c.model.table_center, c.model.slots, c.model.nose_above_table), (m.table, m.table_center, m.slots, m.nose_above_table));
        assert_eq!(c.buttons, Buttons::default());
    }

    #[test]
    fn mymill_profile_reads_clean() {
        // mymill's ferrum.toml, beside Ferrum's folder; not there when
        // Ferrum is checked out on its own.
        let Ok(text) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ferrum.toml")) else { return };
        let c = parse(&text);
        assert_eq!(c.toolchange.style, Some(ToolChangeStyle::Drawbar));
        assert_eq!(c.probe.tool, Some(99));
        assert!(c.model.known());
        assert!(c.module_ids(&Config::default(), "").contains(&"drawbar".to_string()));
    }

    #[test]
    fn no_file_draws_no_table() {
        let c = MachineCfg::default();
        assert!(!c.model.show && !c.model.known());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let c = parse("[buttons]\nflood = false\n[model]\nslots = 4\n");
        assert!(!c.buttons.flood && c.buttons.mist);
        assert_eq!(c.model.slots, 4);
        assert_eq!(c.model.table, Machine::pm728vt().table);
    }

    #[test]
    fn a_typo_resets_only_its_table() {
        let (c, p) = MachineCfg::parse("[buttons]\nflod = false\n[model]\nslots = 4\n");
        assert_eq!(p.len(), 1);
        assert!(p[0].starts_with("[buttons]") && p[0].contains("flod"), "{p:?}");
        assert_eq!(c.buttons, Buttons::default());
        assert_eq!(c.model.slots, 4);
        let (_, p) = MachineCfg::parse("[probes]\ntool = 3\n");
        assert_eq!(p, ["unknown table [probes] (ignored)"]);
    }

    #[test]
    fn park_and_words() {
        assert_eq!(parse("[machine]\npark = [10, -20]\n").machine.park, Park::Xy(10.0, -20.0));
        assert_eq!(parse("[machine]\npark = \"g28\"\n").machine.park, Park::G28);
        assert!(!MachineCfg::parse("[machine]\npark = \"home\"\n").1.is_empty());
        let ids = |t| parse(t).module_ids(&Config::default(), "run");
        assert_eq!(ids("[modules]\nenabled = \"run, manual files\"\n"), ["run", "manual", "files"]);
        assert_eq!(ids("[modules]\nenabled = [\"run\", \"util\"]\n"), ["run", "util"]);
        assert_eq!(ids(""), ["run"]);
    }

    #[test]
    fn toml_wins_over_the_ini() {
        let mut ini = Config::default();
        ini.insert("FERRUM", "MODULES", "run files");
        ini.insert("FERRUM", "REMOTE_PORT", "80x");
        ini.insert("PROBE", "TOOL_NUMBER", "7");
        ini.insert("SPINDLE_0", "MAX_FORWARD_VELOCITY", "3000");
        ini.insert("SPINDLE_0", "MAX_REVERSE_VELOCITY", "0");
        let c = MachineCfg::default();
        assert_eq!(c.module_ids(&ini, "x"), ["run", "files"]);
        assert!(c.remote_port(&ini).unwrap().is_err());
        assert_eq!((c.probe_tool(&ini), c.max_rpm(&ini), c.reverse(&ini)), (7, Some(3000.0), false));
        let c = parse("[modules]\nenabled = \"run\"\n[remote]\nport = 8088\n[probe]\ntool = 12\n[spindle]\nmax_rpm = 2000\nreverse = true\n");
        assert_eq!(c.module_ids(&ini, "x"), ["run"]);
        assert_eq!(c.remote_port(&ini), Some(Ok(8088)));
        assert_eq!((c.probe_tool(&ini), c.max_rpm(&ini), c.reverse(&ini)), (12, Some(2000.0), true));
        assert!(!c.is_scratch_tool(200));
        assert!(parse("[toolchange]\nscratch_tools = [200, 249]\n").is_scratch_tool(249));
    }

    #[test]
    fn nested_keys_are_written_under_their_own_header() {
        let out = edited("", "probe.defaults.slow_feed", Some(toml_edit::value(40.0))).unwrap();
        assert_eq!(out, "[probe.defaults]\nslow_feed = 40.0\n");
        let out = edited("[probe]\ntool = 7\n", "probe.defaults.max_z", Some(toml_edit::value(12.5))).unwrap();
        assert_eq!(MachineCfg::parse(&out).0.probe.defaults.max_z, Some(12.5));
        assert_eq!(MachineCfg::parse(&out).0.probe.tool, Some(7));
        let out = edited(&out, "probe.defaults.max_z", None).unwrap();
        assert_eq!(MachineCfg::parse(&out).0.probe.defaults.max_z, None);
        assert!(edited(&out, "probe.defaults.max_zz", Some(toml_edit::value(1.0))).is_err());
    }
}
