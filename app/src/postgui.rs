//! One-time conversion of the INI's `[HAL]POSTGUI_HALFILE` (and
//! `POSTGUI_HALCMD`) for ferrum. Those files are written for another
//! screen's pins (AXIS's pyvcp panel, QtDragon, Gmoccapy, Probe Basic), so
//! ferrum does not run them. The conversion keeps every line that is about
//! the machine, drops what only another screen understands, and says why
//! for each dropped line; the kept lines go to `ferrum-postgui.hal`, listed
//! in `[display]postgui`. ferrum.toml is kept as `ferrum.toml.bak`; the old
//! files are not touched (another screen may still use them).

use crate::config::Config;
use std::path::{Path, PathBuf};

/// Pin prefixes of other screens and their panels.
const SCREEN_PINS: &[&str] = &[
    "pyvcp.",
    "axisui.",
    "gmoccapy.",
    "gscreen.",
    "qtdragon.",
    "qtvcp.",
    "qtpyvcp.",
    "qtpyvcp_manualtoolchange.",
    "qtversaprobe.",
    "qtbasicprobe.",
    "hal_manualtoolchange.",
    "gladevcp.",
    "touchy.",
    "probe_basic.",
    "tklinuxcnc.",
    "qtplasmac.",
];

/// Why a pin of another screen needs nothing in ferrum.
fn why(pin: &str) -> &'static str {
    let p = pin.to_ascii_lowercase();
    if p.contains("toolchange") || p.contains("tool-change") || p.ends_with(".change") || p.ends_with(".changed") || p.ends_with(".number") {
        "ferrum answers M6 itself (or leaves it to what in HAL already does)"
    } else if p.contains("probe") {
        "ferrum reads motion.probe-input"
    } else if p.contains("spindle") || p.contains("rpm") {
        "ferrum reads spindle.0.speed-in and spindle.0.at-speed"
    } else if p.contains("timer") {
        "ferrum times runs itself"
    } else if p.contains("tooloffset") {
        "ferrum shows tool offsets itself"
    } else {
        "a pin of another screen"
    }
}

fn is_screen_pin(tok: &str) -> bool {
    SCREEN_PINS.iter().any(|p| tok.starts_with(p))
}

/// One line of the old file and what happens to it.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    /// Copied as is (or with another screen's pins taken off a `net`).
    Keep(String),
    /// Left out, with the reason.
    Drop(String, String),
}

/// Convert one file's text.
pub fn convert_text(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let code = line.split('#').next().unwrap_or("").trim();
        if code.is_empty() {
            out.push(Line::Keep(raw.to_string()));
            continue;
        }
        let w: Vec<&str> = code.split_whitespace().collect();
        let screen: Vec<&str> = w.iter().copied().filter(|t| is_screen_pin(t)).collect();
        if screen.is_empty() {
            // `loadusr hal_manualtoolchange` and the like: another screen's program.
            if w[0].starts_with("loadusr") && w.iter().any(|t| t.contains("hal_manualtoolchange")) {
                out.push(Line::Drop(raw.to_string(), "ferrum has its own tool change dialog".into()));
            } else {
                out.push(Line::Keep(raw.to_string()));
            }
            continue;
        }
        let reason = why(screen[0]).to_string();
        match w[0] {
            "net" if w.len() > 2 => {
                // Keep the net while a pin of the machine is left on it: a
                // reader joining a signal the main HAL drives needs it, and
                // a lone writer on a new signal does no harm.
                let rest: Vec<&str> = w[2..].iter().copied().filter(|t| !is_screen_pin(t)).collect();
                let pins = rest.iter().filter(|t| !matches!(**t, "<=" | "=>" | "<=>")).count();
                if pins >= 1 {
                    let mut kept = vec!["net", w[1]];
                    kept.extend(rest.iter().copied());
                    // A dangling arrow at the end ("net s a.b =>") is fine for halcmd; tidy it.
                    while kept.last().is_some_and(|t| matches!(*t, "<=" | "=>" | "<=>")) {
                        kept.pop();
                    }
                    out.push(Line::Keep(format!("{}  # was: {code} ({reason}, so ferrum drops {})", kept.join(" "), screen.join(", "))));
                } else {
                    out.push(Line::Drop(raw.to_string(), reason));
                }
            }
            _ => out.push(Line::Drop(raw.to_string(), reason)),
        }
    }
    out
}

/// What the INI names: its postgui files (relative to the INI) and its
/// POSTGUI_HALCMD lines.
pub fn sources(cfg: &Config) -> (Vec<String>, Vec<String>) {
    let files = cfg.get_all("HAL", "POSTGUI_HALFILE").into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let cmds = cfg.get_all("HAL", "POSTGUI_HALCMD").into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    (files, cmds)
}

/// The conversion is still to do: the INI names postgui files or commands
/// and ferrum.toml does not say they were converted.
pub fn pending(cfg: &Config, mc: &crate::machine_cfg::MachineCfg) -> Option<Vec<String>> {
    let (files, cmds) = sources(cfg);
    if files.is_empty() && cmds.is_empty() {
        return None;
    }
    let done = mc.display.postgui_converted.as_ref().map(|w| w.0.clone()).unwrap_or_default();
    let mut all = files;
    if !cmds.is_empty() {
        all.push("POSTGUI_HALCMD".into());
    }
    (!all.iter().all(|f| done.contains(f))).then_some(all)
}

/// The output file's name, beside the INI.
pub const OUT: &str = "ferrum-postgui.hal";

/// What a conversion did, for the message line and `--check`.
#[derive(Debug, Default)]
pub struct Report {
    pub kept: usize,
    pub dropped: Vec<(String, String)>,
    pub wrote: Option<PathBuf>,
    pub backup: Option<PathBuf>,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut s = match &self.wrote {
            Some(p) => format!("Kept {} HAL lines in {}", self.kept, p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())),
            None => "Nothing of the old postgui file is needed".into(),
        };
        if !self.dropped.is_empty() {
            s.push_str(&format!("; left out {} line(s) for other screens", self.dropped.len()));
        }
        if self.backup.is_some() {
            s.push_str("; the old ferrum.toml is ferrum.toml.bak");
        }
        s
    }
}

/// Convert, write `ferrum-postgui.hal` beside the INI when anything is
/// kept, and record the conversion in ferrum.toml (backed up first).
pub fn convert(cfg: &Config, toml_path: &Path, seed: Option<&Path>) -> Result<Report, String> {
    let (files, cmds) = sources(cfg);
    let mut body = String::new();
    let mut report = Report::default();
    for f in &files {
        let p = cfg.dir().join(f);
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{f}: {e}"))?;
        body.push_str(&format!("\n# From {f}\n"));
        for l in convert_text(&text) {
            match l {
                Line::Keep(l) => {
                    if !l.trim().is_empty() && !l.trim().starts_with('#') {
                        report.kept += 1;
                    }
                    body.push_str(&l);
                    body.push('\n');
                }
                Line::Drop(l, why) => {
                    body.push_str(&format!("# ferrum: dropped ({why}): {}\n", l.trim()));
                    report.dropped.push((l, why));
                }
            }
        }
    }
    if !cmds.is_empty() {
        body.push_str("\n# From [HAL]POSTGUI_HALCMD\n");
        for c in &cmds {
            for l in convert_text(c) {
                match l {
                    Line::Keep(l) => {
                        report.kept += 1;
                        body.push_str(&l);
                        body.push('\n');
                    }
                    Line::Drop(l, why) => {
                        body.push_str(&format!("# ferrum: dropped ({why}): {}\n", l.trim()));
                        report.dropped.push((l, why));
                    }
                }
            }
        }
    }
    let mut converted: Vec<String> = files.clone();
    if !cmds.is_empty() {
        converted.push("POSTGUI_HALCMD".into());
    }
    if toml_path.exists() {
        let bak = toml_path.with_extension("toml.bak");
        std::fs::copy(toml_path, &bak).map_err(|e| format!("{}: {e}", bak.display()))?;
        report.backup = Some(bak);
    }
    if report.kept > 0 {
        let out = cfg.dir().join(OUT);
        if out.exists() {
            let bak = out.with_extension("hal.bak");
            std::fs::copy(&out, &bak).map_err(|e| format!("{}: {e}", bak.display()))?;
        }
        let head = "# The machine's own postgui HAL lines, converted once for ferrum from the\n\
                    # INI's [HAL]POSTGUI_HALFILE. Lines only another screen understood are left\n\
                    # out (commented below). ferrum runs this file through [display]postgui.\n";
        std::fs::write(&out, format!("{head}{body}")).map_err(|e| format!("{}: {e}", out.display()))?;
        let mc = crate::machine_cfg::get();
        let mut list = mc.postgui(cfg);
        if !list.iter().any(|f| f == OUT) {
            list.push(OUT.into());
        }
        let mut arr = toml_edit::Array::new();
        list.iter().for_each(|f| arr.push(f.as_str()));
        crate::machine_cfg::write_key(toml_path, seed, "display.postgui", Some(toml_edit::value(arr)))?;
        report.wrote = Some(out);
    }
    let mut arr = toml_edit::Array::new();
    converted.iter().for_each(|f| arr.push(f.as_str()));
    crate::machine_cfg::write_key(toml_path, seed, "display.postgui_converted", Some(toml_edit::value(arr)))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_basic_postgui() {
        let pb = "loadrt time\naddf time.0 servo-thread\nnet cycle-seconds qtpyvcp.timerseconds.in <= time.0.seconds\n\
                  net tool-change-request     =>  qtpyvcp_manualtoolchange.change\n\
                  net probe-in  <=  qtpyvcp.probe-led.on\n\
                  setp hm2_7i76e.0.7i76.0.0.output_source 0x0000F7FF\n# a comment\n";
        let out = convert_text(pb);
        assert_eq!(out[0], Line::Keep("loadrt time".into()));
        assert!(matches!(&out[2], Line::Keep(l) if l.starts_with("net cycle-seconds <= time.0.seconds  # was:") && l.contains("times runs")), "{:?}", out[2]);
        assert!(matches!(&out[3], Line::Drop(_, w) if w.contains("M6")));
        assert!(matches!(&out[4], Line::Drop(_, w) if w.contains("probe")));
        assert_eq!(out[5], Line::Keep("setp hm2_7i76e.0.7i76.0.0.output_source 0x0000F7FF".into()));
    }

    #[test]
    fn nets_keep_the_machine_side() {
        // gmoccapy_postgui.hal: the at-speed LED only; the abs component feeding the bar keeps its input.
        let g = "net spindle-at-speed      gmoccapy.spindle_at_speed_led\n\
                 net spindle-abs  abs_spindle_feedback.out  =>  gmoccapy.spindle_feedback_bar\n\
                 net tool-change-loop  iocontrol.0.tool-change      =>  iocontrol.0.tool-changed\n\
                 net mpg-x  mpg.out => axis.x.jog-counts pyvcp.mpg-display\n\
                 loadusr -W hal_manualtoolchange\n";
        let out = convert_text(g);
        assert!(matches!(&out[0], Line::Drop(..)));
        assert!(matches!(&out[1], Line::Keep(l) if l.starts_with("net spindle-abs abs_spindle_feedback.out  #")), "{:?}", out[1]);
        assert!(matches!(&out[2], Line::Keep(l) if l.starts_with("net tool-change-loop")));
        assert!(matches!(&out[3], Line::Keep(l) if l.starts_with("net mpg-x mpg.out => axis.x.jog-counts  #")), "{:?}", out[3]);
        assert!(matches!(&out[4], Line::Drop(_, w) if w.contains("tool change dialog")));
    }
}
