//! Cutting load record. While a program runs and a load signal is wired
//! (the `load` module, `ferrum.load.value`), the load is summed per program
//! line, per tool and per job step. When the run ends the record is saved
//! under the state folder (`loads/`, one JSON file per run) for a feeds
//! calculator such as chipwise to read, and the backplot's legend shows
//! the run's peak and where it was.
//!
//! With no load signal nothing is sampled, saved or shown: `sample` is
//! only called with a reading.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A load at or over this counts as hot (seconds over it are kept).
pub const HOT: f32 = 0.8;
/// Lines kept in a saved record, the highest peaks first.
const KEEP_LINES: usize = 100;
/// Records kept in the folder; older ones are removed.
const KEEP_FILES: usize = 200;
/// A sample stands for at most this long (a stalled tick is not cutting).
const MAX_DT: f64 = 0.5;

/// One reading, taken while the program runs.
#[derive(Clone, Debug)]
pub struct Sample {
    /// Program line executing (1-based).
    pub line: i32,
    pub tool: i32,
    /// Job step running, when the program is a job.
    pub op: Option<String>,
    /// 0 to 1.
    pub load: f32,
    /// Feed now, machine units per minute.
    pub feed: f64,
    pub rpm: f64,
    /// Seconds since the previous sample.
    pub dt: f64,
}

#[derive(Clone, Debug, Default)]
struct Acc {
    peak: f32,
    secs: f64,
    load_s: f64,
    hot_s: f64,
    feed_s: f64,
    rpm_s: f64,
    tool: i32,
}

impl Acc {
    fn add(&mut self, s: &Sample, dt: f64) {
        self.peak = self.peak.max(s.load);
        self.secs += dt;
        self.load_s += s.load as f64 * dt;
        if s.load >= HOT {
            self.hot_s += dt;
        }
        self.feed_s += s.feed * dt;
        self.rpm_s += s.rpm.abs() * dt;
        self.tool = s.tool;
    }
    fn mean(&self, v: f64) -> f64 {
        if self.secs > 0.0 { v / self.secs } else { 0.0 }
    }
    fn stats(&self) -> Stats {
        Stats {
            peak: r3(self.peak as f64),
            mean: r3(self.mean(self.load_s)),
            seconds: r1(self.secs),
            hot_seconds: r1(self.hot_s),
            feed: r1(self.mean(self.feed_s)),
            rpm: self.mean(self.rpm_s).round(),
        }
    }
}

fn r1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}
fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Load over a stretch of the run: peak and time-weighted mean (0 to 1),
/// seconds sampled, seconds at or over `HOT`, and the mean feed (machine
/// units per minute) and spindle speed while sampled.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stats {
    pub peak: f64,
    pub mean: f64,
    pub seconds: f64,
    pub hot_seconds: f64,
    pub feed: f64,
    pub rpm: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Peak {
    pub load: f64,
    pub line: i32,
    pub tool: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolStats {
    pub tool: i32,
    #[serde(flatten)]
    pub stats: Stats,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpStats {
    pub op: String,
    #[serde(flatten)]
    pub stats: Stats,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LineStats {
    pub line: i32,
    /// The line's G-code, when the program could be read.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    pub tool: i32,
    #[serde(flatten)]
    pub stats: Stats,
}

/// A finished run's record, as saved.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub format: &'static str,
    pub version: u32,
    pub program: String,
    /// Local time the run started, RFC 3339.
    pub started: String,
    /// Machine units: "mm" or "in" (feeds are these per minute).
    pub units: String,
    /// The last line that ran: compare it with the program's end to tell
    /// a full run from one stopped part way.
    pub last_line: i32,
    pub peak: Peak,
    pub overall: Stats,
    pub tools: Vec<ToolStats>,
    pub ops: Vec<OpStats>,
    /// The lines with the highest peaks, highest first (at most 100).
    pub hot_lines: Vec<LineStats>,
}

impl Report {
    /// The legend's text after the run: "PEAK 92% · LINE 212 · T3".
    pub fn caption(&self) -> String {
        let mut s = format!("PEAK {:.0}%  ·  LINE {}", self.peak.load * 100.0, self.peak.line);
        if self.peak.tool > 0 {
            s += &format!("  ·  T{}", self.peak.tool);
        }
        s
    }
}

struct Run {
    program: String,
    last_line: i32,
    started: String,
    all: Acc,
    peak: Option<Peak>,
    lines: BTreeMap<i32, Acc>,
    tools: BTreeMap<i32, Acc>,
    ops: Vec<(String, Acc)>,
}

/// Collects one run at a time.
#[derive(Default)]
pub struct Recorder {
    run: Option<Run>,
}

impl Recorder {
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// The program the current run belongs to.
    pub fn program(&self) -> Option<&str> {
        self.run.as_ref().map(|r| r.program.as_str())
    }

    /// Start a run of `program` (drops any unfinished one).
    pub fn start(&mut self, program: &str, started: String) {
        self.run = Some(Run {
            program: program.to_string(),
            last_line: 0,
            started,
            all: Acc::default(),
            peak: None,
            lines: BTreeMap::new(),
            tools: BTreeMap::new(),
            ops: Vec::new(),
        });
    }

    pub fn sample(&mut self, s: Sample) {
        let Some(run) = self.run.as_mut() else { return };
        if !s.load.is_finite() || s.line <= 0 {
            return;
        }
        let dt = s.dt.clamp(0.0, MAX_DT);
        run.last_line = s.line;
        run.all.add(&s, dt);
        if run.peak.as_ref().is_none_or(|p| (s.load as f64) > p.load) {
            run.peak = Some(Peak { load: s.load as f64, line: s.line, tool: s.tool, op: s.op.clone() });
        }
        run.lines.entry(s.line).or_default().add(&s, dt);
        run.tools.entry(s.tool).or_default().add(&s, dt);
        if let Some(op) = &s.op {
            match run.ops.iter_mut().find(|(n, _)| n == op) {
                Some((_, a)) => a.add(&s, dt),
                None => {
                    let mut a = Acc::default();
                    a.add(&s, dt);
                    run.ops.push((op.clone(), a));
                }
            }
        }
    }

    /// End the run. `None` when nothing was sampled. `lines` gives a
    /// line's text (1-based) for the hot lines.
    pub fn finish(&mut self, units: &str, lines: impl Fn(i32) -> String) -> Option<Report> {
        let run = self.run.take()?;
        let peak = run.peak?;
        let mut hot: Vec<(&i32, &Acc)> = run.lines.iter().collect();
        // Highest peak first; a tie in program order, so the first is the run's peak line.
        hot.sort_by(|a, b| b.1.peak.total_cmp(&a.1.peak).then(a.0.cmp(b.0)));
        hot.truncate(KEEP_LINES);
        Some(Report {
            format: "ferrum-load",
            version: 1,
            program: run.program,
            started: run.started,
            units: units.to_string(),
            last_line: run.last_line,
            peak: Peak { load: r3(peak.load), ..peak },
            overall: run.all.stats(),
            tools: run.tools.iter().map(|(t, a)| ToolStats { tool: *t, stats: a.stats() }).collect(),
            ops: run.ops.iter().map(|(o, a)| OpStats { op: o.clone(), stats: a.stats() }).collect(),
            hot_lines: hot.into_iter().map(|(l, a)| LineStats { line: *l, text: lines(*l), tool: a.tool, stats: a.stats() }).collect(),
        })
    }
}

/// Where records go: `<state>/loads`.
pub fn dir() -> Option<PathBuf> {
    crate::settings::Settings::dir().map(|d| d.join("loads"))
}

/// The file name for a record: `20261119-171500-part.ngc.json`.
fn file_name(r: &Report, stamp: &str) -> String {
    let base = Path::new(&r.program).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "program".into());
    let base: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    format!("{stamp}-{base}.json")
}

/// Save `r` in `dir`; returns the file written. Keeps the newest records.
pub fn save(dir: &Path, r: &Report, stamp: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name(r, stamp));
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(r).unwrap_or_default())?;
    std::fs::rename(&tmp, &path)?;
    prune(dir, KEEP_FILES);
    Ok(path)
}

fn prune(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    if files.len() <= keep {
        return;
    }
    // Names start with the time, so they sort oldest first.
    files.sort();
    for f in &files[..files.len() - keep] {
        let _ = std::fs::remove_file(f);
    }
}

/// Drives a `Recorder` from the screen's 200 ms tick.
pub struct Tracker {
    rec: Recorder,
    last: Option<std::time::Instant>,
    dir: Option<PathBuf>,
    /// The program the legend's peak is for, and the line it was on.
    shown: Option<(String, i32)>,
}

impl Tracker {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { rec: Recorder::default(), last: None, dir, shown: None }
    }

    /// Where finished runs are saved (None: not saved).
    pub fn set_dir(&mut self, dir: Option<PathBuf>) {
        self.dir = dir;
    }

    /// The peak's line, for a tap on the legend.
    pub fn peak_line(&self) -> Option<i32> {
        self.shown.as_ref().map(|s| s.1)
    }

    /// `program` is the loaded program, `load` the reading now (None: no
    /// load signal), `op` the job step running.
    pub fn tick(&mut self, ui: &crate::MainWindow, s: Option<&machine::Status>, program: &str, load: Option<f64>, op: impl FnOnce() -> Option<String>) {
        use slint::ComponentHandle;
        let now = std::time::Instant::now();
        let dt = self.last.map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        self.last = Some(now);
        // A program running in AUTO (not an MDI line or a probe macro).
        let running = s.is_some_and(|s| s.mode == machine::Mode::Auto && s.busy()) && !program.is_empty();
        if running && load.is_some() && self.rec.program() != Some(program) {
            self.rec.start(program, chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
            self.show(ui, None);
        }
        if !running {
            if self.rec.running() {
                let text = ui.global::<crate::Live>().get_gcode_lines();
                let line = |l: i32| slint::Model::row_data(&text, l as usize - 1).map(|t| t.trim().to_string()).unwrap_or_default();
                let units = match crate::machine_info::get().units {
                    crate::machine_info::Units::Mm => "mm",
                    crate::machine_info::Units::Inch => "in",
                };
                if let Some(r) = self.rec.finish(units, line) {
                    if let Some(dir) = &self.dir {
                        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
                        if let Err(e) = save(dir, &r, &stamp) {
                            eprintln!("ferrum: could not save the load record in {}: {e}", dir.display());
                        }
                    }
                    self.show(ui, Some((r.caption(), r.program.clone(), r.peak.line)));
                }
            }
            // Another program loaded: its peak is not this one's.
            if self.shown.as_ref().is_some_and(|(p, _)| p != program) {
                self.show(ui, None);
            }
            return;
        }
        let (Some(s), Some(load)) = (s, load) else { return };
        // A subroutine's lines (the M6 remap, a probe macro) are not the program's.
        if s.interp == machine::Interp::Paused || s.file != program {
            return;
        }
        let line = if s.motion_line > 0 { s.motion_line } else { s.current_line };
        self.rec.sample(Sample { line, tool: s.tool_in_spindle, op: op(), load: load as f32, feed: s.feed_per_min(), rpm: s.spindle_speed, dt });
    }

    fn show(&mut self, ui: &crate::MainWindow, peak: Option<(String, String, i32)>) {
        use slint::ComponentHandle;
        let live = ui.global::<crate::Live>();
        let text = peak.as_ref().map(|p| p.0.clone()).unwrap_or_default();
        if live.get_plot_load_peak() != text.as_str() {
            live.set_plot_load_peak(text.into());
        }
        self.shown = peak.map(|(_, p, l)| (p, l));
    }

    /// Screenshots: show a peak as a finished run would.
    pub fn show_peak(&mut self, ui: &crate::MainWindow, program: &str, r: &Report) {
        self.show(ui, Some((r.caption(), program.to_string(), r.peak.line)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(line: i32, tool: i32, load: f32, op: Option<&str>) -> Sample {
        Sample { line, tool, op: op.map(String::from), load, feed: 600.0, rpm: 3000.0, dt: 0.2 }
    }

    #[test]
    fn nothing_sampled_is_no_record() {
        let mut r = Recorder::default();
        assert!(r.finish("mm", |_| String::new()).is_none());
        r.start("/p/a.ngc", "t".into());
        assert!(r.running());
        assert!(r.finish("mm", |_| String::new()).is_none());
        assert!(!r.running());
    }

    #[test]
    fn sums_per_line_tool_and_op() {
        let mut r = Recorder::default();
        r.start("/p/a.ngc", "t".into());
        r.sample(s(10, 3, 0.2, Some("FACE")));
        r.sample(s(10, 3, 0.4, Some("FACE")));
        r.sample(s(20, 3, 0.9, Some("FACE")));
        r.sample(s(30, 5, 0.5, Some("DRILL")));
        // Unreadable samples are skipped.
        r.sample(s(0, 5, 0.99, None));
        r.sample(s(31, 5, f32::NAN, None));
        let rep = r.finish("mm", |l| format!("G1 L{l}")).unwrap();
        assert_eq!(rep.peak, Peak { load: 0.9, line: 20, tool: 3, op: Some("FACE".into()) });
        assert_eq!(rep.caption(), "PEAK 90%  ·  LINE 20  ·  T3");
        assert_eq!(rep.overall.seconds, 0.8);
        assert_eq!(rep.overall.hot_seconds, 0.2);
        assert_eq!(rep.overall.mean, 0.5);
        assert_eq!(rep.tools.iter().map(|t| (t.tool, t.stats.peak)).collect::<Vec<_>>(), vec![(3, 0.9), (5, 0.5)]);
        assert_eq!(rep.ops.iter().map(|o| o.op.as_str()).collect::<Vec<_>>(), vec!["FACE", "DRILL"]);
        let lines: Vec<(i32, f64, &str)> = rep.hot_lines.iter().map(|l| (l.line, l.stats.peak, l.text.as_str())).collect();
        assert_eq!(lines, vec![(20, 0.9, "G1 L20"), (30, 0.5, "G1 L30"), (10, 0.4, "G1 L10")]);
        assert_eq!(rep.hot_lines[2].stats.mean, 0.3);
    }

    #[test]
    fn a_stalled_tick_counts_at_most_half_a_second() {
        let mut r = Recorder::default();
        r.start("a.ngc", "t".into());
        r.sample(Sample { dt: 30.0, ..s(1, 1, 0.5, None) });
        assert_eq!(r.finish("in", |_| String::new()).unwrap().overall.seconds, 0.5);
    }

    #[test]
    fn records_are_saved_and_pruned() {
        let dir = std::env::temp_dir().join(format!("ferrum-loads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = Recorder::default();
        r.start("/x/my part.ngc", "2026-11-19T17:00:00-08:00".into());
        r.sample(s(4, 2, 0.6, None));
        let rep = r.finish("mm", |_| String::new()).unwrap();
        let path = save(&dir, &rep, "20261119-170000").unwrap();
        assert_eq!(path.file_name().unwrap(), "20261119-170000-my_part.ngc.json");
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["format"], "ferrum-load");
        assert_eq!(v["hot_lines"][0]["line"], 4);
        assert_eq!(v["hot_lines"][0]["peak"], 0.6);
        assert!(v["peak"].get("op").is_none());
        for i in 0..5 {
            std::fs::write(dir.join(format!("2026010{i}-000000-a.json")), "{}").unwrap();
        }
        prune(&dir, 3);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["20260103-000000-a.json", "20260104-000000-a.json", "20261119-170000-my_part.ngc.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
