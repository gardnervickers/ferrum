//! Reads the LinuxCNC INI the way LinuxCNC does: `#INCLUDE` is expanded in
//! place and a lookup returns the first matching key in the first matching
//! section (later sections with the same name are never searched).

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Config {
    pub path: Option<PathBuf>,
    /// (section, key, value) in file order, after #INCLUDE expansion.
    entries: Vec<(String, String, String)>,
    /// Sections in order of first appearance; lookups only see the first.
    first_section_index: Vec<(String, usize)>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut lines = Vec::new();
        expand(path, &mut lines, 0)?;
        let mut cfg = Config { path: Some(path.to_path_buf()), ..Default::default() };
        let mut section = String::new();
        let mut section_no = 0usize;
        let mut seen: Vec<String> = Vec::new();
        for raw in lines {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.split(']').next()) {
                section = name.trim().to_string();
                section_no += 1;
                if !seen.contains(&section) {
                    seen.push(section.clone());
                    cfg.first_section_index.push((section.clone(), section_no));
                }
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                // Tag the entry with its section occurrence so only the first counts.
                cfg.entries.push((format!("{section}\u{0}{section_no}"), k.trim().to_string(), strip_comment(v)));
            }
        }
        Ok(cfg)
    }

    /// Test helper: add a key to a section.
    #[cfg(test)]
    pub fn insert(&mut self, section: &str, key: &str, value: &str) {
        if self.section_tag(section).is_none() {
            let n = self.first_section_index.len() + 1;
            self.first_section_index.push((section.to_string(), n));
        }
        let tag = self.section_tag(section).unwrap();
        self.entries.push((tag, key.to_string(), value.to_string()));
    }

    fn section_tag(&self, section: &str) -> Option<String> {
        self.first_section_index
            .iter()
            .find(|(s, _)| s == section)
            .map(|(s, n)| format!("{s}\u{0}{n}"))
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.get_all(section, key).into_iter().next()
    }

    pub fn get_all(&self, section: &str, key: &str) -> Vec<&str> {
        let Some(tag) = self.section_tag(section) else { return Vec::new() };
        self.entries.iter().filter(|(s, k, _)| *s == tag && k == key).map(|(_, _, v)| v.as_str()).collect()
    }

    #[allow(dead_code)] // used by later modules
    pub fn get_bool(&self, section: &str, key: &str) -> Option<bool> {
        self.get(section, key).map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
    }

    pub fn get_f64(&self, section: &str, key: &str) -> Option<f64> {
        self.get(section, key).and_then(|v| v.parse().ok())
    }

    /// Axis letters from [TRAJ]COORDINATES, e.g. "XYZA".
    pub fn coordinates(&self) -> String {
        self.get("TRAJ", "COORDINATES").unwrap_or("XYZ").chars().filter(|c| !c.is_whitespace()).collect()
    }

    pub fn dir(&self) -> PathBuf {
        self.path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    }
}

fn strip_comment(v: &str) -> String {
    // LinuxCNC keeps everything after '='; trailing "# ..." comments are not
    // stripped by inivar either, but values here never contain '#'.
    v.split(" #").next().unwrap_or(v).trim().to_string()
}

fn expand(path: &Path, out: &mut Vec<String>, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err(format!("#INCLUDE nested too deeply at {}", path.display()));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("#INCLUDE") {
            let name = rest.trim();
            let inc = if name.starts_with('/') { PathBuf::from(name) } else { dir.join(name) };
            expand(&inc, out, depth + 1)?;
        } else {
            out.push(line.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_section_wins_and_include_expands() {
        let d = std::env::temp_dir().join(format!("millgui-cfg-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("base.ini"), "[DISPLAY]\nDISPLAY = probe\nCYCLE_TIME = 100\n[TRAJ]\nCOORDINATES = X Y Z A\n").unwrap();
        fs::write(d.join("top.ini"), "[DISPLAY]\nDISPLAY = ./millgui/run\n\n#INCLUDE base.ini\n").unwrap();
        let c = Config::load(&d.join("top.ini")).unwrap();
        assert_eq!(c.get("DISPLAY", "DISPLAY"), Some("./millgui/run"));
        assert_eq!(c.get("DISPLAY", "CYCLE_TIME"), None);
        assert_eq!(c.coordinates(), "XYZA");
        fs::remove_dir_all(d).ok();
    }
}
