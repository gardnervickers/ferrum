//! Backplot on the Run screen: the loaded program's toolpath from the
//! interpreter (in the background), the live tool position and its trail.

use crate::config::Config;
use crate::{Actions, CubeFace, Live, MainWindow};
use preview::machine::{Machine, Pose};
use preview::software::{Base, View, Zoom};
use preview::preflight::{self, Limits, Report};
use preview::{rs274, Toolpath, P4};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

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
    file: String,
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
    /// Soft limits from the INI, for the travel check (None: no check).
    limits: Option<Limits>,
    /// The machine's offsets now, and the travel check of the toolpath
    /// with them (None until a program is read and status arrived).
    live: Option<preflight::Live>,
    check: Option<Report>,
    summary: String,
}

/// The preview INI: the machine INI without REMAP lines. Remaps read HAL
/// pins, which do not exist for the stand-alone interpreter; the built-in
/// M6 is enough for a preview.
fn setup(cfg: &Config) -> Option<rs274::Setup> {
    if !rs274::available() || cfg.path.is_none() {
        return None;
    }
    let dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(".cache/millgui");
    std::fs::create_dir_all(&dir).ok()?;
    let ini = dir.join("preview.ini");
    let text: String = cfg
        .expanded_lines()
        .into_iter()
        .map(|l| if l.trim_start().starts_with("REMAP") { format!("# millgui preview: {l}") } else { l })
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

fn rapid(cfg: &Config) -> f64 {
    cfg.get_f64("TRAJ", "MAX_LINEAR_VELOCITY").unwrap_or(100.0)
}

impl Plot {
    fn redraw(&mut self, ui: &MainWindow) {
        self.dirty = false;
        self.version += 1;
        self.pixels = None;
        let (w, h) = self.size;
        if w < 16 || h < 16 {
            return;
        }
        if self.gpu && !self.inline {
            self.gpu_frame = true;
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
                    std::thread::spawn(move || {
                        let t0 = std::time::Instant::now();
                        let b = Base::draw(&tp, key.1, key.2, key.3, None, key.4);
                        if perf() {
                            eprintln!("millgui perf: backplot software base {}x{}, {} moves: {:.1} ms", key.2, key.3, tp.segs.len(), t0.elapsed().as_secs_f64() * 1e3);
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
            eprintln!("millgui perf: backplot software compose {}x{}: {:.1} ms", w, h, t0.elapsed().as_secs_f64() * 1e3);
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
        let top = -self.machine.nose_above_table;
        Some(preflight::check(&self.tp, limits, live, Some(top), from_line))
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
        let mut text = preflight::describe(p);
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
    let r = st.check_from(from_line)?;
    let p = r.travel().next()?;
    let mut text = preflight::describe(p);
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
            format!("{}{n}", preflight::describe(p))
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

/// Read `path` through the interpreter (or the stand-in) on a worker
/// thread; `done` runs on the UI thread with the result.
pub fn analyze(plot: &Rc<RefCell<Plot>>, path: PathBuf, done: impl FnOnce(Toolpath) + Send + 'static) {
    if plot.borrow().inline {
        // Headless screenshots have no event loop to come back on.
        let rapid = plot.borrow().rapid;
        done(std::fs::read_to_string(&path).map(|t| preview::mini::parse(&t, rapid)).unwrap_or_default());
        return;
    }
    let (setup, rapid) = {
        let p = plot.borrow();
        (p.setup.as_ref().map(|s| rs274::Setup { ini: s.ini.clone(), config_dir: s.config_dir.clone(), var_file: s.var_file.clone(), tool_table: s.tool_table.clone(), rapid_mm_s: s.rapid_mm_s }), p.rapid)
    };
    std::thread::spawn(move || {
        let tp = match &setup {
            Some(s) => rs274::run(s, &path),
            None => match std::fs::read_to_string(&path) {
                Ok(t) => preview::mini::parse(&t, rapid),
                Err(e) => Toolpath { error: Some(e.to_string()), ..Default::default() },
            },
        };
        let _ = slint::invoke_from_event_loop(move || done(tp));
    });
}

/// Build the toolpath for `file` off the UI thread.
pub fn load(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, file: &str) {
    let weak = ui.as_weak();
    let path = PathBuf::from(file);
    {
        let mut p = plot.borrow_mut();
        p.trail.clear();
        p.trail_load.clear();
        p.set_toolpath(ui, Toolpath::default());
    }
    if file.is_empty() {
        return;
    }
    ui.global::<Live>().set_plot_summary("READING PROGRAM…".into());
    analyze(plot, path, move |tp| {
        if let Some(ui) = weak.upgrade() {
            with(|p| p.borrow_mut().set_toolpath(&ui, tp));
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
    let far = st.trail.last().is_none_or(|l| (0..3).map(|i| (l[i] - p[i]).abs()).fold(0.0, f64::max) > 0.05);
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
    live.set_cube_faces(slint::ModelRc::new(slint::VecModel::from(faces)));
}

thread_local! {
    static PLOT: RefCell<Option<Rc<RefCell<Plot>>>> = const { RefCell::new(None) };
}

pub fn wire(ui: &MainWindow, cfg: &Config, model: &crate::machine_cfg::Model) -> Rc<RefCell<Plot>> {
    ui.global::<Live>().set_plot_machine(model.show);
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
        show_machine: model.show,
        dirty: false,
        base: None,
        pending: None,
        turn: slint::Timer::default(),
        busy: false,
        inline: false,
        setup: setup(cfg),
        rapid: rapid(cfg),
        file: String::new(),
        stamp: (String::new(), None, 0),
        gpu: false,
        gpu_frame: false,
        want_pixels: false,
        pixels: None,
        version: 0,
        limits: limits(cfg),
        live: None,
        check: None,
        summary: String::new(),
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

/// Redraw at most ten times a second while the tool moves.
pub fn start_timer(ui: &MainWindow, plot: &Rc<RefCell<Plot>>) -> slint::Timer {
    let t = slint::Timer::default();
    let (p, weak) = (plot.clone(), ui.as_weak());
    t.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        if st.dirty {
            st.redraw(&ui);
        }
    });
    t
}

/// Synchronous load for headless screenshots.
pub fn load_now(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, file: &str) {
    let rapid = plot.borrow().rapid;
    plot.borrow_mut().inline = true;
    plot.borrow_mut().file = file.to_string();
    let tp = std::fs::read_to_string(file).map(|t| preview::mini::parse(&t, rapid)).unwrap_or_default();
    plot.borrow_mut().set_toolpath(ui, tp);
}

/// Run `f` on the plot, if the Run screen's backplot is wired.
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

thread_local! {
    static GPU: RefCell<Option<preview::gpu::Gpu>> = const { RefCell::new(None) };
}

/// Draw the backplot with OpenGL when Slint renders with femtovg. Falls back
/// to the software backplot if the GL setup fails.
pub fn use_gpu(ui: &MainWindow) -> Result<(), slint::SetRenderingNotifierError> {
    let weak = ui.as_weak();
    ui.window().set_rendering_notifier(move |state, api| {
        let Some(ui) = weak.upgrade() else { return };
        match (state, api) {
            (slint::RenderingState::RenderingSetup, slint::GraphicsAPI::NativeOpenGL { get_proc_address }) => {
                match unsafe { preview::gpu::Gpu::new(get_proc_address) } {
                    Ok(g) => {
                        eprintln!("millgui: backplot on the GPU: {}", g.describe());
                        GPU.with(|c| *c.borrow_mut() = Some(g));
                        with(|p| {
                            let mut st = p.borrow_mut();
                            st.gpu = true;
                            st.base = None;
                            st.dirty = true;
                        });
                    }
                    Err(e) => eprintln!("millgui: GPU backplot unavailable ({e}); drawing it in software"),
                }
            }
            (slint::RenderingState::BeforeRendering, _) => with(|p| gpu_frame(&ui, p)),
            (slint::RenderingState::RenderingTeardown, _) => {
                GPU.with(|c| *c.borrow_mut() = None);
                with(|p| {
                    let mut st = p.borrow_mut();
                    st.gpu = false;
                    st.dirty = true;
                });
            }
            _ => {}
        }
    })
}

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
                eprintln!("millgui perf: backplot GPU frame {w}x{h}, {} moves: {:.1} ms", st.tp.segs.len(), t0.elapsed().as_secs_f64() * 1e3);
            }
            if let Some(px) = px {
                st.want_pixels = false;
                st.pixels = Some(SharedPixelBuffer::clone_from_slice(&px, w.max(16), h.max(16)));
            }
            let size = [w.max(16), h.max(16)].into();
            let img = unsafe { slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(tex.0, size) }
                .origin(slint::BorrowedOpenGLTextureOrigin::BottomLeft)
                .build();
            let live = ui.global::<Live>();
            live.set_plot(img);
            if live.get_plot_pan_seq() != st.pan_seq {
                live.set_plot_pan_seq(st.pan_seq);
                live.set_plot_landed(live.get_plot_landed() + 1);
            }
        }
        Some(Err(e)) => {
            eprintln!("millgui: GPU backplot failed ({e}); drawing it in software");
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

/// `MILLGUI_PERF=1` logs backplot frame times.
fn perf() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("MILLGUI_PERF").is_some())
}
