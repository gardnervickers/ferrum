//! Operator settings remembered between runs (power feed values, GOTO
//! locations, jog speed): `key = value` lines in
//! `$MILLGUI_STATE` or `~/.local/state/millgui/settings`. Never machine config.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Settings {
    path: Option<PathBuf>,
    values: BTreeMap<String, String>,
}

impl Settings {
    pub fn load() -> Self {
        let path = std::env::var_os("MILLGUI_STATE").map(PathBuf::from).or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/millgui/settings"))
        });
        let mut values = BTreeMap::new();
        if let Some(text) = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    values.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
        Self { path, values }
    }

    /// Another state file next to the settings one (`probe` → `.../probe`).
    pub fn at(name: &str) -> Self {
        let dir = std::env::var_os("MILLGUI_STATE").map(PathBuf::from).and_then(|p| p.parent().map(PathBuf::from)).or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/millgui"))
        });
        let mut s = Self { path: dir.map(|d| d.join(name)), values: BTreeMap::new() };
        if let Some(text) = s.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    s.values.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
        s
    }

    /// In-memory only (screenshots and tests).
    pub fn memory() -> Self {
        Self { path: None, values: BTreeMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn f64(&self, key: &str, default: f64) -> f64 {
        self.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn set(&mut self, key: &str, value: impl ToString) {
        self.values.insert(key.to_string(), value.to_string());
        self.save();
    }

    /// Replace every key that starts with `prefix`.
    pub fn set_list(&mut self, prefix: &str, items: impl IntoIterator<Item = String>) {
        self.values.retain(|k, _| !k.starts_with(prefix));
        for (i, v) in items.into_iter().enumerate() {
            self.values.insert(format!("{prefix}{i:03}"), v);
        }
        self.save();
    }

    pub fn list(&self, prefix: &str) -> Vec<&str> {
        self.values.iter().filter(|(k, _)| k.starts_with(prefix)).map(|(_, v)| v.as_str()).collect()
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text: String = self.values.iter().map(|(k, v)| format!("{k} = {v}\n")).collect();
        let tmp = path.with_extension("tmp");
        let ok = path.parent().map_or(Ok(()), std::fs::create_dir_all).is_ok()
            && std::fs::write(&tmp, text).is_ok()
            && std::fs::rename(&tmp, path).is_ok();
        if !ok {
            eprintln!("millgui: could not save {}", path.display());
        }
    }
}
