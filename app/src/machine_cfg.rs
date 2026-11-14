//! machine.toml: per-machine settings that are not LinuxCNC's (the INI
//! stays Probe Basic's). Missing keys take the built-in defaults.

use crate::config::Config;
use crate::{Live, MainWindow};
use preview::machine::Machine;
use serde::Deserialize;
use slint::ComponentHandle;

#[cfg(test)]
const BUILTIN: &str = include_str!("../../machine.toml");

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
        Model {
            show: true,
            table: m.table,
            table_center: m.table_center,
            slots: m.slots,
            nose_above_table: m.nose_above_table,
        }
    }
}

impl Model {
    pub fn machine(&self) -> Machine {
        Machine {
            table: self.table,
            table_center: self.table_center,
            slots: self.slots,
            nose_above_table: self.nose_above_table,
        }
    }
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MachineCfg {
    pub buttons: Buttons,
    pub model: Model,
}

impl MachineCfg {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// `[MILLGUI]MACHINE_CONFIG`, else `millgui/machine.toml` beside the
    /// INI, else the defaults. A parse error is noted and the defaults used.
    pub fn load(cfg: &Config, notes: &mut Vec<String>) -> Self {
        let path = cfg
            .get("MILLGUI", "MACHINE_CONFIG")
            .map(|p| cfg.dir().join(p))
            .unwrap_or_else(|| cfg.dir().join("millgui/machine.toml"));
        match std::fs::read_to_string(&path) {
            Ok(t) => Self::parse(&t).unwrap_or_else(|e| {
                notes.push(format!("{}: {e}", path.display()));
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn apply(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        live.set_quick_mist(self.buttons.mist);
        live.set_quick_flood(self.buttons.flood);
        live.set_quick_drawbar(self.buttons.drawbar);
        live.set_quick_home(self.buttons.home_all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_file_is_the_defaults() {
        assert_eq!(MachineCfg::parse(BUILTIN).unwrap(), MachineCfg::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let c = MachineCfg::parse("[buttons]\nflood = false\n[model]\nslots = 4\n").unwrap();
        assert!(!c.buttons.flood && c.buttons.mist);
        assert_eq!(c.model.slots, 4);
        assert_eq!(c.model.table, Machine::pm728vt().table);
    }

    #[test]
    fn typo_is_an_error() {
        assert!(MachineCfg::parse("[buttons]\nflod = false\n").is_err());
    }
}
