//! Backplot on the Run screen: the loaded program's toolpath from the
//! interpreter (in the background), the live tool position and its trail.

use crate::config::Config;
use crate::{Actions, CubeFace, Live, MainWindow};
use preview::machine::{Machine, Pose};
use preview::software::{Base, View, Zoom};
use preview::preflight::{self, Limits, Report};
use preview::{rs274, timing, Toolpath, P4};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const TRAIL_MAX: usize = 50_000;

/// What a base image was drawn for: toolpath id, view, size.
type Key = (usize, View, u32, u32, Zoom);

pub struct Plot {
    tp: Arc<Toolpath>,
    id: usize,
    view: View,
    zoom: Zoom,
    /// The number of the latest pan asked for.
    pan_seq: i32,
    size: (u32, u32),
    trail: Vec<P4>,
    /// Cutting load at each trail point (NaN: none), and the latest reading.
    trail_load: Vec<f32>,
    load: Option<f32>,
    tool: Option<P4>,
    /// The machine model drawn around the toolpath, and where it is.
    machine: Machine,
    pose: Option<Pose>,
    show_machine: bool,
    dirty: bool,
    /// The drawn toolpath, and the one being drawn on a worker thread. The
    /// last base stays on screen until its replacement is ready.
    base: Option<(Key, Base)>,
    pending: Option<Key>,
    /// Turns the view to a view-cube snap.
    turn: slint::Timer,
    /// A worker thread is drawing a base.
    busy: bool,
    /// Draw inline (headless screenshots).
    inline: bool,
    setup: Option<rs274::Setup>,
    rapid: f64,
    /// How the rotary turns the drawing (see `preview::wrap_a`).
    a_sign: f64,
    file: String,
    /// Counts program reads (`load`); a read's result is used only if no
    /// later read started, so a slow read of the program before cannot
    /// replace the one now loaded.
    reads: u64,
    /// A read is under way: there is no travel check yet.
    reading: bool,
    /// The file's path, time and size when it was read (see program::stale).
    stamp: crate::program::Stamp,
    /// Slint draws with OpenGL and the GPU backplot is ready: frames are
    /// drawn in the rendering notifier instead of on the CPU.
    gpu: bool,
    /// A GPU frame is wanted at the next render.
    gpu_frame: bool,
    /// The web mirror asked for the GPU frame's pixels.
    want_pixels: bool,
    pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    /// Counts redraws, so the web mirror knows when the picture changed.
    version: u64,
    /// When it was last redrawn (redraws are paced, see `redraw_due`).
    drawn_at: Option<Instant>,
    /// The GL texture and size the image property shows (GPU backplot).
    gpu_shown: Option<(u32, [u32; 2])>,
    /// Soft limits from the INI, for the travel check (None: no check).
    limits: Option<Limits>,
    /// The machine's offsets now, and the travel check of the toolpath
    /// with them (None until a program is read and status arrived).
    live: Option<preflight::Live>,
    check: Option<Report>,
    summary: String,
    /// The machine's speed and acceleration limits, for the run-time
    /// estimate (`preview::timing`).
    timing: timing::Limits,
    /// Where the running program is along the toolpath (None: not running).
    tracker: Option<timing::Tracker>,
    /// Ops the run skips (a JOB's turned-off ops), by index.
    skip: Vec<bool>,
    /// The feed and rapid overrides at the last status.
    scales: (f64, f64),
}

/// The preview INI: the machine INI without REMAP lines. Remaps read HAL
/// pins, which do not exist for the stand-alone interpreter; the built-in
/// M6 is enough for a preview.
fn setup(cfg: &Config) -> Option<rs274::Setup> {
    if !rs274::available() || cfg.path.is_none() {
        return None;
    }
    let dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(".cache/ferrum");
    std::fs::create_dir_all(&dir).ok()?;
    let ini = dir.join("preview.ini");
    let text: String = cfg
        .expanded_lines()
        .into_iter()
        .map(|l| if l.trim_start().starts_with("REMAP") { format!("# ferrum preview: {l}") } else { l })
        .map(|l| l + "\n")
        .collect();
    std::fs::write(&ini, text).ok()?;
    let rel = |p: &str| {
        let p = PathBuf::from(p);
        if p.is_absolute() { p } else { cfg.dir().join(p) }
    };
    Some(rs274::Setup {
        ini,
        config_dir: cfg.dir(),
        var_file: cfg.get("RS274NGC", "PARAMETER_FILE").map(rel),
        tool_table: cfg.get("EMCIO", "TOOL_TABLE").map(rel),
        rapid_mm_s: rapid(cfg),
        inch: crate::machine_info::get().units.inch(),
    })
}

/// [AXIS_X/Y/Z] MIN_LIMIT and MAX_LIMIT, when all six are set.
pub fn limits(cfg: &Config) -> Option<Limits> {
    let mut l = Limits { min: [0.0; 3], max: [0.0; 3] };
    for (i, a) in ["X", "Y", "Z"].iter().enumerate() {
        let sec = format!("AXIS_{a}");
        l.min[i] = cfg.get_f64(&sec, "MIN_LIMIT")?;
        l.max[i] = cfg.get_f64(&sec, "MAX_LIMIT")?;
    }
    Some(l)
}

/// The rotary's sense for drawing: [DISPLAY]GEOMETRY as LinuxCNC's AXIS
/// reads it ("XYZA" turns the drawing with A, "-A" the other way). Without
/// GEOMETRY, a machine with an A axis wraps the plain way.
fn a_sign(cfg: &Config) -> f64 {
    match cfg.get("DISPLAY", "GEOMETRY") {
        Some(g) => preview::geometry_a_sign(g),
        None if cfg.get("TRAJ", "COORDINATES").is_some_and(|c| c.to_ascii_uppercase().contains('A')) => 1.0,
        None => 0.0,
    }
}

/// The limits LinuxCNC's planner works to, from the INI (machine units).
fn timing_limits(cfg: &Config) -> timing::Limits {
    let mut l = timing::Limits::default();
    for (i, a) in ["X", "Y", "Z", "A"].iter().enumerate() {
        let sec = format!("AXIS_{a}");
        l.vel[i] = cfg.get_f64(&sec, "MAX_VELOCITY").unwrap_or(if i < 3 { l.vel[i] } else { 0.0 });
        l.acc[i] = cfg.get_f64(&sec, "MAX_ACCELERATION").unwrap_or(if i < 3 { l.acc[i] } else { 0.0 });
    }
    l.max_linear_vel = cfg.get_f64("TRAJ", "MAX_LINEAR_VELOCITY").unwrap_or(0.0);
    l.max_linear_acc = cfg.get_f64("TRAJ", "MAX_LINEAR_ACCELERATION").unwrap_or(0.0);
    l.servo_period = cfg.get_f64("EMCMOT", "SERVO_PERIOD").map_or(0.001, |ns| ns * 1e-9);
    l.depth = cfg.get_f64("TRAJ", "ARC_BLEND_OPTIMIZATION_DEPTH").map_or(50, |d| d.max(0.0) as usize);
    l.io_wait = cfg.get_f64("EMCIO", "CYCLE_TIME").map_or(l.io_wait, |c| c * 1.2);
    l
}

/// Machine units per second.
fn rapid(cfg: &Config) -> f64 {
    cfg.get_f64("TRAJ", "MAX_LINEAR_VELOCITY").unwrap_or(crate::machine_info::get().mm(100.0))
}

impl Plot {
    /// Point the backplot image at a GPU texture (no change if it already is).
    #[cfg(not(target_os = "wasi"))]
    fn show_gpu_texture(&mut self, ui: &MainWindow, tex: std::num::NonZeroU32, size: [u32; 2]) {
        if self.gpu_shown == Some((tex.get(), size)) {
            return;
        }
        self.gpu_shown = Some((tex.get(), size));
        let img = unsafe { slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(tex, size.into()) }
            .origin(slint::BorrowedOpenGLTextureOrigin::BottomLeft)
            .build();
        ui.global::<Live>().set_plot(img);
    }

    fn redraw(&mut self, ui: &MainWindow) {
        self.dirty = false;
        self.drawn_at = Some(Instant::now());
        self.version += 1;
        self.pixels = None;
        let (w, h) = self.size;
        if w < 16 || h < 16 {
            return;
        }
        #[cfg(not(target_os = "wasi"))]
        if self.gpu && !self.inline {
            self.gpu_frame = true;
            // Draw into the other texture and show it: Slint draws the image
            // again only when the property changes. Set here, before the
            // frame, the change costs no frame of its own.
            let next = GPU.with(|c| {
                let mut c = c.borrow_mut();
                let g = c.as_mut()?;
                g.slot ^= 1;
                g.texture(g.slot).map(|(t, size)| (t.0, size))
            });
            if let Some((tex, size)) = next.filter(|n| n.1 == [w, h]) {
                self.show_gpu_texture(ui, tex, size);
            }
            ui.window().request_redraw();
            return;
        }
        let key = (self.id, self.view, w, h, self.zoom);
        if self.base.as_ref().map(|b| b.0) != Some(key) {
            if self.inline {
                self.base = Some((key, Base::draw(&self.tp, self.view, w, h, None, self.zoom)));
            } else {
                // One drawing at a time: on the Pi a drag asks for many, and
                // parallel ones would starve the UI thread. The latest wish
                // is drawn when the current one finishes.
                self.pending = Some(key);
                if !self.busy {
                    self.busy = true;
                    let tp = self.tp.clone();
                    let weak = ui.as_weak();
                    crate::util::background(move || {
                        let t0 = std::time::Instant::now();
                        let b = Base::draw(&tp, key.1, key.2, key.3, None, key.4);
                        if perf() {
                            eprintln!("ferrum perf: backplot software base {}x{}, {} moves: {:.1} ms", key.2, key.3, tp.segs.len(), t0.elapsed().as_secs_f64() * 1e3);
                        }
                        let _ = slint::invoke_from_event_loop(move || {
                            with(|p| {
                                let mut st = p.borrow_mut();
                                st.busy = false;
                                if st.pending == Some(key) {
                                    st.pending = None;
                                    st.base = Some((key, b));
                                }
                                if let Some(ui) = weak.upgrade() {
                                    st.redraw(&ui);
                                }
                            })
                        });
                    });
                }
            }
        }
        let Some((drawn, base)) = &self.base else { return };
        let scene = self.pose.map(|p| self.machine.scene(&p, self.show_machine));
        let t0 = std::time::Instant::now();
        let pm = base.compose(&self.trail, &self.trail_load, self.tool, scene.as_ref());
        if perf() {
            eprintln!("ferrum perf: backplot software compose {}x{}: {:.1} ms", w, h, t0.elapsed().as_secs_f64() * 1e3);
        }
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(pm.data(), pm.width(), pm.height());
        let live = ui.global::<Live>();
        live.set_plot(Image::from_rgba8(buf));
        // The image now includes the latest pan: the screens drop the part
        // of their drag preview offset that it covers.
        if *drawn == key && live.get_plot_pan_seq() != self.pan_seq {
            live.set_plot_pan_seq(self.pan_seq);
            live.set_plot_landed(live.get_plot_landed() + 1);
        }
    }

    /// With no moves, frame the table so it and the tool still show.
    fn empty_frame(&self) -> (P4, P4) {
        self.machine.frame(self.pose.map(|p| p.offset).unwrap_or_default())
    }

    fn set_toolpath(&mut self, ui: &MainWindow, mut tp: Toolpath) {
        if tp.segs.is_empty() {
            tp.frame = Some(self.empty_frame());
        }
        let live = ui.global::<Live>();
        let secs = tp.seconds();
        let ops = tp.ops.len();
        self.summary = if tp.segs.is_empty() {
            String::new()
        } else {
            format!("{} OP{} · ~{}", ops, if ops == 1 { "" } else { "S" }, fmt_time(secs))
        };
        live.set_plot_summary(self.summary.clone().into());
        // The estimate shows until a run starts (`progress` takes over).
        self.skip.clear();
        if self.tracker.is_none() {
            live.set_time_label("TIME".into());
            live.set_time_left(if tp.t_end.is_empty() || secs <= 0.0 { String::new() } else { fmt_clock(secs) }.into());
            live.set_time_fraction(-1.0);
        }
        if let Some(e) = &tp.error {
            live.set_message(format!("Preview: {e}").into());
            live.set_message_error(true);
        }
        self.tp = Arc::new(tp);
        self.id += 1;
        self.zoom = Zoom::default();
        self.run_check(ui);
        self.redraw(ui);
    }

    /// The travel check of the toolpath with the offsets now, from line
    /// `from_line` on.
    fn check_from(&self, from_line: u32) -> Option<Report> {
        let (limits, live) = (self.limits.as_ref()?, self.live.as_ref()?);
        if self.tp.segs.is_empty() {
            return None;
        }
        // No table check until the table is described.
        let top = (self.machine.nose_above_table > 0.0).then(|| -self.machine.nose_above_table);
        Some(preflight::check(&self.tp, limits, live, top, from_line))
    }

    fn run_check(&mut self, ui: &MainWindow) {
        self.check = self.check_from(0);
        let live = ui.global::<Live>();
        let (text, level, line) = match &self.check {
            None => (String::new(), 0, 0),
            Some(r) => check_text(r, &self.tp, self.live.as_ref()),
        };
        live.set_plot_check(text.into());
        live.set_plot_check_level(level);
        live.set_plot_check_line(line as i32);
    }
}

/// The banner text, its level (0 OK, 1 note, 2 problem) and the line to
/// show for a travel check.
fn check_text(r: &Report, tp: &Toolpath, live: Option<&preflight::Live>) -> (String, i32, u32) {
    let wcs = live.map_or(0, |l| l.active.clamp(1, 9) as usize - 1);
    let op_name = |op: usize| tp.ops.get(op).map(|o| o.name.clone()).filter(|n| !n.is_empty());
    if let Some(p) = r.problems.first() {
        let mut text = preflight::describe(p, crate::machine_info::get().units.inch());
        if let Some(n) = op_name(p.op) {
            text += &format!(" ({n})");
        }
        if r.problems.len() > 1 {
            text += &format!(" · {} MORE", r.problems.len() - 1);
        }
        let level = if r.travel().next().is_some() { 2 } else { 1 };
        return (text, level, p.line);
    }
    if let Some(line) = r.sets_offsets.first() {
        let text = if tp.find_lines.contains(line) {
            format!("OK to line {line} · the job probes its offset from there")
        } else {
            format!("OK with today's offsets · sets its own from line {line}")
        };
        return (text, 1, *line);
    }
    (format!("OK in {} · every move inside the limits", crate::offsets::CODES[wcs]), 0, 0)
}

/// Why the loaded program must not start from line `from_line` (0: the
/// top): a move past a soft limit. LinuxCNC would stop there mid-cut.
/// A tool tip below the table only warns (the model is approximate).
pub fn start_problem(plot: &Rc<RefCell<Plot>>, from_line: u32) -> Option<String> {
    let st = plot.borrow();
    if st.reading {
        // Until the read is done there is nothing to check: a start now
        // would skip the travel check.
        return Some("The program is still being read for the travel check; start again when the preview shows".into());
    }
    let r = st.check_from(from_line)?;
    let p = r.travel().next()?;
    let mut text = preflight::describe(p, crate::machine_info::get().units.inch());
    if let Some(n) = st.tp.ops.get(p.op).map(|o| o.name.clone()).filter(|n| !n.is_empty()) {
        text += &format!(" ({n})");
    }
    Some(format!("{text}. Check the work offset"))
}

/// The machine's offsets changed (or arrived): check again.
pub fn set_live(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, live: preflight::Live) {
    let mut st = plot.borrow_mut();
    if st.live == Some(live) {
        return;
    }
    st.live = Some(live);
    st.run_check(ui);
}

/// Every finding of the travel check, one per line, for the message drawer.
pub fn check_details(plot: &Rc<RefCell<Plot>>) -> Option<(String, u32)> {
    let st = plot.borrow();
    let r = st.check.as_ref()?;
    let mut lines: Vec<String> = r
        .problems
        .iter()
        .map(|p| {
            let n = st.tp.ops.get(p.op).map(|o| format!(" ({})", o.name)).unwrap_or_default();
            format!("{}{n}", preflight::describe(p, crate::machine_info::get().units.inch()))
        })
        .collect();
    if let Some(l) = r.sets_offsets.first() {
        lines.push(if st.tp.find_lines.contains(l) {
            format!("The job probes its work offset from line {l}; moves after it are not checked, because where they land is not known until the probe has run")
        } else {
            format!("The program sets a work offset or G92 itself from line {l}; moves after it were checked with today's values")
        });
    }
    let line = r.problems.first().map(|p| p.line).or(r.sets_offsets.first().copied()).unwrap_or(0);
    Some((lines.join("\n"), line))
}

pub fn fmt_time(s: f64) -> String {
    let m = s / 60.0;
    if m < 1.0 {
        format!("{:.0} s", s)
    } else if m < 60.0 {
        format!("{:.0} min", m.ceil())
    } else {
        format!("{} h {:.0} min", (m / 60.0) as u32, m % 60.0)
    }
}

/// A time as a clock: "4:05", "1:02:30".
pub fn fmt_clock(s: f64) -> String {
    let s = s.max(0.0).ceil() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A JOB's ops the run will skip (turned off), by op id.
pub fn set_skip(plot: &Rc<RefCell<Plot>>, off: &std::collections::HashSet<String>) {
    let mut st = plot.borrow_mut();
    st.skip = st.tp.ops.iter().map(|o| !o.id.is_empty() && off.contains(&o.id)).collect();
}

/// Times of a JOB program's ops, by op id (estimates at 100 %), and the
/// running op: its id, its share done (by time) and its time left with
/// today's overrides. Empty when the loaded program has no JOB ops.
pub struct JobTimes {
    pub by_id: std::collections::HashMap<String, f64>,
    pub now: Option<(String, f64, f64)>,
}

pub fn job_times(plot: &Rc<RefCell<Plot>>) -> JobTimes {
    let st = plot.borrow();
    let tp = &st.tp;
    let timed = !tp.t_end.is_empty();
    let by_id = if timed { tp.ops.iter().filter(|o| !o.id.is_empty()).map(|o| (o.id.clone(), o.seconds)).collect() } else { Default::default() };
    let now = st.tracker.as_ref().filter(|_| timed).and_then(|t| {
        let at100 = t.left(tp, 1.0, 1.0, &st.skip);
        let left = t.left(tp, st.scales.0, st.scales.1, &st.skip);
        let op = tp.ops.get(left.op).filter(|o| !o.id.is_empty())?;
        let frac = if op.seconds > 0.0 { (1.0 - at100.op_left / op.seconds).clamp(0.0, 1.0) } else { 0.0 };
        Some((op.id.clone(), frac, left.op_left))
    });
    JobTimes { by_id, now }
}

/// Counts toolpath changes (a new program read), so pages showing its
/// estimates know when to look again.
pub fn toolpath_id(plot: &Rc<RefCell<Plot>>) -> u64 {
    plot.borrow().id as u64
}

/// The time left in the whole run, with today's overrides (None: not running).
pub fn run_left(plot: &Rc<RefCell<Plot>>) -> Option<f64> {
    let st = plot.borrow();
    let t = st.tracker.as_ref()?;
    Some(t.left(&st.tp, st.scales.0, st.scales.1, &st.skip).total)
}

/// What a running program has left, from the line motion is on and the
/// tool's position; `running` is false when no program runs (MDI included).
/// Sets the Run screen's time and progress, and the op line over the
/// backplot.
pub fn progress(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, running: bool, line: u32, feed_scale: f64, rapid_scale: f64) {
    let mut st = plot.borrow_mut();
    st.scales = (feed_scale, rapid_scale);
    let live = ui.global::<Live>();
    let tp = st.tp.clone();
    let timed = !tp.t_end.is_empty() && tp.t_end.len() == tp.segs.len();
    if !running || !timed {
        st.tracker = None;
        let total = if timed { timing::total(&tp, 1.0, 1.0, &st.skip) } else { 0.0 };
        set_if(live.get_time_label() != "TIME", || live.set_time_label("TIME".into()));
        let text = if timed && total > 0.0 { fmt_clock(total) } else { String::new() };
        if live.get_time_left() != text.as_str() {
            live.set_time_left(text.into());
        }
        if live.get_time_fraction() != -1.0 {
            live.set_time_fraction(-1.0);
        }
        if live.get_plot_summary() != st.summary.as_str() && live.get_plot_summary() != "READING PROGRAM…" {
            live.set_plot_summary(st.summary.clone().into());
        }
        return;
    }
    let near = crate::machine_info::get().units.pick(2.0, 0.08);
    let tool = st.tool.unwrap_or_default();
    let mut tracker = st.tracker.take().unwrap_or_default();
    tracker.update(&tp, line, tool, near);
    let left = tracker.left(&tp, feed_scale, rapid_scale, &st.skip);
    let at = tracker.at;
    st.tracker = Some(tracker);
    set_if(live.get_time_label() != "LEFT", || live.set_time_label("LEFT".into()));
    let text = fmt_clock(left.total);
    if live.get_time_left() != text.as_str() {
        live.set_time_left(text.into());
    }
    // In steps of 0.2 %: the bar must not cost a frame every status tick.
    let full = tp.t_end.last().copied().unwrap_or(0.0) as f64;
    let frac = if full > 0.0 { ((at / full).clamp(0.0, 1.0) * 500.0).round() as f32 / 500.0 } else { 0.0 };
    if live.get_time_fraction() != frac {
        live.set_time_fraction(frac);
    }
    let n = tp.ops.len();
    let name = tp.ops.get(left.op).map(|o| o.name.clone()).unwrap_or_default();
    let summary = if n > 1 {
        format!("OP {}/{n} · {} · {} LEFT IN OP", left.op + 1, name, fmt_clock(left.op_left))
    } else {
        format!("{} LEFT", fmt_clock(left.total))
    };
    if live.get_plot_summary() != summary.as_str() {
        live.set_plot_summary(summary.into());
    }
}

fn set_if(cond: bool, f: impl FnOnce()) {
    if cond {
        f()
    }
}

/// Read `path` through the interpreter (or the stand-in) on a worker
/// thread; `done` runs on the UI thread with the result.
pub fn analyze(plot: &Rc<RefCell<Plot>>, path: PathBuf, done: impl FnOnce(Toolpath) + Send + 'static) {
    let a_sign = plot.borrow().a_sign;
    let lim = plot.borrow().timing.clone();
    let done = move |mut tp: Toolpath| {
        tp.set_rotary(a_sign);
        done(tp)
    };
    if plot.borrow().inline {
        // Headless screenshots have no event loop to come back on.
        let rapid = plot.borrow().rapid;
        let mut tp = std::fs::read_to_string(&path).map(|t| preview::mini::parse(&t, rapid)).unwrap_or_default();
        timing::estimate(&mut tp, &lim);
        done(tp);
        return;
    }
    let (setup, rapid) = {
        let p = plot.borrow();
        (p.setup.as_ref().map(|s| rs274::Setup { ini: s.ini.clone(), config_dir: s.config_dir.clone(), var_file: s.var_file.clone(), tool_table: s.tool_table.clone(), rapid_mm_s: s.rapid_mm_s, inch: s.inch }), p.rapid)
    };
    crate::util::background(move || {
        // Always answer, even if the read panics: a start waits for it.
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut tp = match &setup {
                Some(s) => rs274::run(s, &path),
                None => match std::fs::read_to_string(&path) {
                    Ok(t) => preview::mini::parse(&t, rapid),
                    Err(e) => Toolpath { error: Some(e.to_string()), ..Default::default() },
                },
            };
            timing::estimate(&mut tp, &lim);
            tp
        }));
        let tp = read.unwrap_or_else(|_| Toolpath { error: Some("the program could not be read for the preview".into()), ..Default::default() });
        let _ = slint::invoke_from_event_loop(move || done(tp));
    });
}

/// Build the toolpath for `file` off the UI thread.
pub fn load(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, file: &str) {
    let weak = ui.as_weak();
    let path = PathBuf::from(file);
    let read = {
        let mut p = plot.borrow_mut();
        p.trail.clear();
        p.trail_load.clear();
        p.set_toolpath(ui, Toolpath::default());
        p.reads += 1;
        p.reading = !file.is_empty();
        p.reads
    };
    if file.is_empty() {
        return;
    }
    ui.global::<Live>().set_plot_summary("READING PROGRAM…".into());
    analyze(plot, path, move |tp| {
        if let Some(ui) = weak.upgrade() {
            with(|p| {
                let mut p = p.borrow_mut();
                if p.reads == read {
                    p.reading = false;
                    p.set_toolpath(&ui, tp);
                }
            });
        }
    });
}

/// Draws inline, without worker threads (headless screenshots).
pub fn inline(plot: &Rc<RefCell<Plot>>) -> bool {
    plot.borrow().inline
}

/// Follow the tool (work coordinates) and extend the trail.
pub fn tool_moved(plot: &Rc<RefCell<Plot>>, p: P4) {
    tool_moved_homed(plot, p, true);
}

/// As `tool_moved`, but positions from before homing (and the jump homing
/// makes) leave no trail: they are not where the tool really was.
pub fn tool_moved_homed(plot: &Rc<RefCell<Plot>>, p: P4, homed: bool) {
    let mut st = plot.borrow_mut();
    if st.tool == Some(p) {
        return;
    }
    if !homed {
        st.trail.clear();
        st.trail_load.clear();
        st.tool = Some(p);
        st.dirty = true;
        return;
    }
    // A turn of the rotary counts too: wrapped, it moves the tool around the part.
    let far = st.trail.last().is_none_or(|l| (0..3).map(|i| (l[i] - p[i]).abs()).fold(0.0, f64::max) > 0.05 || (l[3] - p[3]).abs() > 0.5);
    if far {
        if st.trail.len() >= TRAIL_MAX {
            st.trail.drain(..TRAIL_MAX / 10);
            st.trail_load.drain(..TRAIL_MAX / 10);
        }
        st.trail.push(p);
        let load = st.load.unwrap_or(f32::NAN);
        st.trail_load.push(load);
    }
    st.tool = Some(p);
    st.dirty = true;
}

/// The cutting load now (0 to 1), from the module that measures it; `None`
/// when there is none. Trail points made from here on carry it.
pub fn set_load(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, load: Option<f64>) {
    let load = load.map(|l| l as f32);
    let mut st = plot.borrow_mut();
    if st.load != load {
        st.load = load;
        let text = load.map(|l| format!("{:.0}%", l * 100.0)).unwrap_or_default();
        let live = ui.global::<Live>();
        if live.get_plot_load() != text.as_str() {
            live.set_plot_load(text.into());
        }
    }
}

/// Where the machine model is (tool length and diameter, work offset).
pub fn machine_pose(plot: &Rc<RefCell<Plot>>, pose: Pose) {
    let mut st = plot.borrow_mut();
    if st.pose != Some(pose) {
        st.pose = Some(pose);
        st.dirty = true;
        // No program: keep the table framed as the work offset changes.
        let frame = Some(st.empty_frame());
        if st.tp.segs.is_empty() && st.tp.frame != frame {
            let mut tp = (*st.tp).clone();
            tp.frame = frame;
            st.tp = Arc::new(tp);
            st.id += 1;
        }
    }
}

/// The caption and the view cube for `v`.
fn show_view(ui: &MainWindow, v: View) {
    let live = ui.global::<Live>();
    live.set_plot_view(format!("{} VIEW", preview::cube::name(v)).into());
    let faces: Vec<CubeFace> = preview::cube::faces(v)
        .into_iter()
        .map(|f| CubeFace {
            label: f.label.into(),
            path: f.path.into(),
            grid: f.grid.into(),
            lx: f.lx as f32,
            ly: f.ly as f32,
            labelled: f.labelled,
            facing: f.facing as f32,
            on: f.on,
        })
        .collect();
    crate::models::sync(live.get_cube_faces(), faces, |m| live.set_cube_faces(m));
}

thread_local! {
    static PLOT: RefCell<Option<Rc<RefCell<Plot>>>> = const { RefCell::new(None) };
}

pub fn wire(ui: &MainWindow, cfg: &Config, model: &crate::machine_cfg::Model) -> Rc<RefCell<Plot>> {
    ui.global::<Live>().set_plot_machine(model.show && model.known());
    ui.global::<Live>().set_plot_has_machine(model.known());
    let plot = Rc::new(RefCell::new(Plot {
        tp: Arc::new(Toolpath::default()),
        id: 0,
        view: View::Iso,
        zoom: Zoom::default(),
        pan_seq: 0,
        size: (0, 0),
        trail: Vec::new(),
        trail_load: Vec::new(),
        load: None,
        tool: None,
        machine: model.machine(),
        pose: None,
        show_machine: model.show && model.known(),
        dirty: false,
        base: None,
        pending: None,
        turn: slint::Timer::default(),
        busy: false,
        inline: false,
        setup: setup(cfg),
        rapid: rapid(cfg),
        a_sign: a_sign(cfg),
        file: String::new(),
        reads: 0,
        reading: false,
        stamp: (String::new(), None, 0),
        // The GPU may have been set up already, while the splash drew.
        gpu: gpu_ready(),
        gpu_frame: false,
        want_pixels: false,
        pixels: None,
        version: 0,
        limits: limits(cfg),
        live: None,
        check: None,
        summary: String::new(),
        timing: timing_limits(cfg),
        tracker: None,
        skip: Vec::new(),
        scales: (1.0, 1.0),
        drawn_at: None,
        gpu_shown: None,
    }));
    PLOT.with(|p| *p.borrow_mut() = Some(plot.clone()));
    let actions = ui.global::<Actions>();
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_resize(move |w, h| {
        let Some(ui) = weak.upgrade() else { return };
        let scale = ui.window().scale_factor();
        let mut st = p.borrow_mut();
        // Held to a size any GPU can make a texture of.
        let size = (((w * scale) as u32).min(4096), ((h * scale) as u32).min(4096));
        if size != st.size {
            st.size = size;
            st.redraw(&ui);
        }
    });
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_view(move |v| {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        st.view = match v.as_str() {
            "top" => View::Top,
            "front" => View::Front,
            _ => View::Iso,
        };
        show_view(&ui, st.view);
        st.redraw(&ui);
    });
    // Orbit: degrees of turn and raise, from dragging the view cube.
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_orbit(move |dyaw, dpitch| {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        st.turn.stop();
        ui.global::<Live>().set_cube_hot("".into());
        st.view = st.view.orbited(dyaw as f64, dpitch as f64);
        show_view(&ui, st.view);
        st.redraw(&ui);
    });
    // A tap on the view cube turns the view to that face, edge or corner.
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_cube_tap(move |x, y| {
        let Some(ui) = weak.upgrade() else { return };
        let st = p.borrow();
        let Some(to) = preview::cube::hit(st.view, x as f64 * 100.0, y as f64 * 100.0) else { return };
        ui.global::<Live>().set_cube_hot("".into());
        let (from, start) = (st.view, std::time::Instant::now());
        let (p2, weak) = (p.clone(), ui.as_weak());
        st.turn.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = p2.borrow_mut();
            let t = (start.elapsed().as_secs_f64() / 0.32).min(1.0);
            // Ease out: quick to leave, gentle to land.
            let e = 1.0 - (1.0 - t).powi(3);
            st.view = if t >= 1.0 { to } else { preview::cube::between(from, to, e) };
            show_view(&ui, st.view);
            st.redraw(&ui);
            if t >= 1.0 {
                st.turn.stop();
            }
        });
    });
    // Light the part of the cube a tap would pick: under the pointer on the
    // Mac, under the finger while pressed. Off the box (x < 0) clears it.
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_cube_hover(move |x, y| {
        let Some(ui) = weak.upgrade() else { return };
        let st = p.borrow();
        let hot = if x < 0.0 { String::new() } else { preview::cube::hot(st.view, x as f64 * 100.0, y as f64 * 100.0) };
        ui.global::<Live>().set_cube_hot(hot.into());
    });
    show_view(ui, View::Iso);
    // Zoom and pan, in logical pixels from the UI. A drag moves the old
    // image at once (plot-dx/dy) and the redraw replaces it.
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_pan(move |dx, dy, seq| {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        let (w, h) = st.size;
        st.zoom = st.zoom.panned(dx as f64 * w as f64, dy as f64 * h as f64);
        st.pan_seq = seq;
        st.redraw(&ui);
    });
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_zoom(move |factor, x, y| {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        let size = st.size;
        st.zoom = st.zoom.zoom_at(factor as f64, (x as f64 * size.0 as f64, y as f64 * size.1 as f64), size);
        st.redraw(&ui);
    });
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_fit(move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        st.zoom = Zoom::default();
        st.redraw(&ui);
    });
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_machine(move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        st.show_machine = !st.show_machine;
        ui.global::<Live>().set_plot_machine(st.show_machine);
        st.redraw(&ui);
    });
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_clear_trail(move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        st.trail.clear();
        st.trail_load.clear();
        st.redraw(&ui);
    });
    plot
}

/// The least time between two redraws: at most ten a second while the tool moves.
const REDRAW_EVERY: Duration = Duration::from_millis(100);

/// Redraw if something changed and the last redraw is old enough. Called
/// from the status handler, so a redraw lands in the same frame as the DRO
/// update instead of costing a frame of its own; the timer covers changes
/// that come without a status (view, pan, an idle machine). Only while the
/// backplot can be seen: on another screen it stays dirty until RUN shows.
pub fn redraw_due(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, slack: Duration) {
    let mut st = plot.borrow_mut();
    if !st.dirty || st.drawn_at.is_some_and(|t| t.elapsed() + slack < REDRAW_EVERY) {
        return;
    }
    if !st.inline && ui.global::<Live>().get_screen() != "run" && !crate::remote::watching() {
        return;
    }
    st.redraw(ui);
}

pub fn start_timer(ui: &MainWindow, plot: &Rc<RefCell<Plot>>) -> slint::Timer {
    let t = slint::Timer::default();
    let (p, weak) = (plot.clone(), ui.as_weak());
    t.start(slint::TimerMode::Repeated, REDRAW_EVERY, move || {
        let Some(ui) = weak.upgrade() else { return };
        redraw_due(&ui, &p, Duration::ZERO);
    });
    t
}

/// Synchronous load for headless screenshots.
pub fn load_now(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, file: &str) {
    let rapid = plot.borrow().rapid;
    plot.borrow_mut().inline = true;
    plot.borrow_mut().file = file.to_string();
    let mut tp = std::fs::read_to_string(file).map(|t| preview::mini::parse(&t, rapid)).unwrap_or_default();
    timing::estimate(&mut tp, &plot.borrow().timing);
    tp.set_rotary(plot.borrow().a_sign);
    plot.borrow_mut().set_toolpath(ui, tp);
}

/// Run `f` on the plot, if the Run screen's backplot is wired.
/// The loaded program's path ("" when none).
pub fn file() -> String {
    let mut f = String::new();
    with(|p| f = p.borrow().file.clone());
    f
}

pub fn with(f: impl FnOnce(&Rc<RefCell<Plot>>)) {
    if let Some(p) = PLOT.with(|p| p.borrow().clone()) {
        f(&p);
    }
}

pub fn with_ret<R>(f: impl FnOnce(&Rc<RefCell<Plot>>) -> R) -> Option<R> {
    PLOT.with(|p| p.borrow().clone()).map(|p| f(&p))
}

/// Reload the toolpath when LinuxCNC reports a different program.
pub fn sync(ui: &MainWindow, file: &str, running: bool) {
    with(|p| {
        let Some(now) = crate::program::stale(&p.borrow().stamp, file, running) else { return };
        let mut st = p.borrow_mut();
        st.file = file.to_string();
        st.stamp = now;
        drop(st);
        load(ui, p, file);
    });
}

/// The GPU backplot is set up (always false in the WASI browser demo).
fn gpu_ready() -> bool {
    #[cfg(not(target_os = "wasi"))]
    return GPU.with(|g| g.borrow().is_some());
    #[cfg(target_os = "wasi")]
    false
}

#[cfg(not(target_os = "wasi"))]
thread_local! {
    static GPU: RefCell<Option<preview::gpu::Gpu>> = const { RefCell::new(None) };
}

/// Draw the backplot with OpenGL when Slint renders with femtovg. Falls back
/// to the software backplot if the GL setup fails.
#[cfg(not(target_os = "wasi"))]
pub fn use_gpu(ui: &MainWindow) -> Result<(), slint::SetRenderingNotifierError> {
    let weak = ui.as_weak();
    let r = ui.window().set_rendering_notifier(move |state, api| {
        let Some(ui) = weak.upgrade() else { return };
        match (state, api) {
            (slint::RenderingState::RenderingSetup, slint::GraphicsAPI::NativeOpenGL { get_proc_address }) => {
                match unsafe { preview::gpu::Gpu::new(get_proc_address) } {
                    Ok(g) => {
                        eprintln!("ferrum: backplot on the GPU: {}", g.describe());
                        GPU.with(|c| *c.borrow_mut() = Some(g));
                        with(|p| {
                            let mut st = p.borrow_mut();
                            st.gpu = true;
                            st.gpu_shown = None;
                            st.base = None;
                            st.dirty = true;
                        });
                    }
                    Err(e) => eprintln!("ferrum: GPU backplot unavailable ({e}); drawing it in software"),
                }
            }
            (slint::RenderingState::BeforeRendering, _) => with(|p| gpu_frame(&ui, p)),
            // The screen watchdog's heartbeat (failsafe.rs).
            (slint::RenderingState::AfterRendering, _) => crate::failsafe::frame_drawn(),
            (slint::RenderingState::RenderingTeardown, _) => {
                GPU.with(|c| *c.borrow_mut() = None);
                with(|p| {
                    let mut st = p.borrow_mut();
                    st.gpu = false;
                    st.gpu_shown = None;
                    st.dirty = true;
                });
            }
            _ => {}
        }
    });
    if r.is_ok() {
        crate::failsafe::notifier_marks_frames();
    }
    r
}

#[cfg(not(target_os = "wasi"))]
fn gpu_frame(ui: &MainWindow, plot: &Rc<RefCell<Plot>>) {
    let mut st = plot.borrow_mut();
    if !st.gpu_frame {
        return;
    }
    st.gpu_frame = false;
    let (w, h) = st.size;
    let scene = st.pose.map(|p| st.machine.scene(&p, st.show_machine));
    let frame = preview::gpu::Frame { view: st.view, zoom: st.zoom, width: w, height: h, trail: &st.trail, loads: &st.trail_load, tool: st.tool, machine: scene.as_ref() };
    let t0 = std::time::Instant::now();
    let drawn = GPU.with(|c| {
        let mut c = c.borrow_mut();
        let g = c.as_mut()?;
        let r = g.render(&st.tp, st.id, &frame).map(|tex| {
            let px = if st.want_pixels { g.read_back(tex, w.max(16), h.max(16)) } else { None };
            (tex, px)
        });
        Some(r)
    });
    match drawn {
        Some(Ok((tex, px))) => {
            if perf() {
                eprintln!("ferrum perf: backplot GPU frame {w}x{h}, {} moves: {:.1} ms", st.tp.segs.len(), t0.elapsed().as_secs_f64() * 1e3);
            }
            if let Some(px) = px {
                st.want_pixels = false;
                st.pixels = Some(SharedPixelBuffer::clone_from_slice(&px, w.max(16), h.max(16)));
            }
            let size = [w.max(16), h.max(16)];
            let live = ui.global::<Live>();
            // Normally `redraw` showed this texture already. The first time
            // (and after a resize) it is new: show it now, which costs one
            // more frame.
            if st.gpu_shown != Some((tex.0.get(), size)) {
                st.show_gpu_texture(ui, tex.0, size);
            }
            if live.get_plot_pan_seq() != st.pan_seq {
                live.set_plot_pan_seq(st.pan_seq);
                live.set_plot_landed(live.get_plot_landed() + 1);
            }
        }
        Some(Err(e)) => {
            eprintln!("ferrum: GPU backplot failed ({e}); drawing it in software");
            st.gpu = false;
            st.dirty = true;
        }
        None => {}
    }
}

/// The backplot's pixels for the web mirror when the image on screen is a
/// GPU texture (which has no CPU copy). Asks for the next frame's pixels.
pub fn gpu_pixels(ui: &MainWindow) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let mut out = None;
    with(|p| {
        let mut st = p.borrow_mut();
        if st.gpu {
            out = st.pixels.take();
            st.want_pixels = true;
            st.gpu_frame = true;
            ui.window().request_redraw();
        }
    });
    out
}

/// Changes whenever the backplot is redrawn.
pub fn version() -> u64 {
    let mut v = 0;
    with(|p| v = p.borrow().version);
    v
}

/// `FERRUM_PERF=1` logs backplot frame times.
fn perf() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FERRUM_PERF").is_some())
}
