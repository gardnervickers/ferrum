//! Backplot on the Run screen: the loaded program's toolpath from the
//! interpreter (in the background), the live tool position and its trail.

use crate::config::Config;
use crate::{Actions, CubeFace, Live, MainWindow};
use preview::machine::{Machine, Pose};
use preview::software::{Base, View, Zoom};
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
    /// Draw inline (headless screenshots).
    inline: bool,
    setup: Option<rs274::Setup>,
    rapid: f64,
    file: String,
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

fn rapid(cfg: &Config) -> f64 {
    cfg.get_f64("TRAJ", "MAX_LINEAR_VELOCITY").unwrap_or(100.0)
}

impl Plot {
    fn redraw(&mut self, ui: &MainWindow) {
        self.dirty = false;
        let (w, h) = self.size;
        if w < 16 || h < 16 {
            return;
        }
        let key = (self.id, self.view, w, h, self.zoom);
        if self.base.as_ref().map(|b| b.0) != Some(key) {
            if self.inline {
                self.base = Some((key, Base::draw(&self.tp, self.view, w, h, None, self.zoom)));
            } else if self.pending.is_none() {
                // One draw in flight: an orbit or a pan asks for a new view
                // every frame, and the Pi draws slower than that. The next
                // starts when this one lands, for the latest view.
                self.pending = Some(key);
                let tp = self.tp.clone();
                std::thread::spawn(move || {
                    let b = Base::draw(&tp, key.1, key.2, key.3, None, key.4);
                    let _ = slint::invoke_from_event_loop(move || {
                        with(|p| {
                            let mut st = p.borrow_mut();
                            if st.pending == Some(key) {
                                st.pending = None;
                                st.base = Some((key, b));
                                st.dirty = true;
                            }
                        })
                    });
                });
            }
        }
        let Some((drawn, base)) = &self.base else { return };
        let scene = self.pose.filter(|_| self.show_machine).map(|p| self.machine.scene(&p));
        let pm = base.compose(&self.trail, &self.trail_load, self.tool, scene.as_ref());
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

    fn set_toolpath(&mut self, ui: &MainWindow, tp: Toolpath) {
        let live = ui.global::<Live>();
        let secs = tp.seconds();
        let ops = tp.ops.len();
        live.set_plot_summary(if tp.segs.is_empty() {
            String::new()
        } else {
            format!("{} OP{} · ~{}", ops, if ops == 1 { "" } else { "S" }, fmt_time(secs))
        }
        .into());
        if let Some(e) = &tp.error {
            live.set_message(format!("Preview: {e}").into());
            live.set_message_error(true);
        }
        self.tp = Arc::new(tp);
        self.id += 1;
        self.zoom = Zoom::default();
        self.redraw(ui);
    }
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

/// One line per op for the FILES screen: name, tool, estimated time.
pub fn op_summary(tp: &Toolpath) -> String {
    if let Some(e) = &tp.error {
        if tp.segs.is_empty() {
            return format!("Cannot read: {e}");
        }
    }
    if tp.segs.is_empty() {
        return "No moves.".into();
    }
    let n = tp.ops.len();
    let mut out = format!("{} OP{} · ~{}", n, if n == 1 { "" } else { "S" }, fmt_time(tp.seconds()));
    for (i, o) in tp.ops.iter().enumerate() {
        let name = if o.name.is_empty() { format!("OP {}", i + 1) } else { o.name.clone() };
        let tag = format!("T{}", o.tool);
        // JOB names already end in their tool; manual steps have none.
        let mut line = format!("\n{}  {}", i + 1, name);
        if o.tool > 0 && !name.ends_with(&tag) {
            line += &format!("  ·  {tag}");
        }
        if o.seconds > 0.0 {
            line += &format!("  ·  ~{}", fmt_time(o.seconds));
        }
        out += &line;
    }
    if let Some(e) = &tp.error {
        out += &format!("\nStops early: {e}");
    }
    out
}

/// Follow the tool (work coordinates) and extend the trail.
pub fn tool_moved(plot: &Rc<RefCell<Plot>>, p: P4) {
    let mut st = plot.borrow_mut();
    if st.tool == Some(p) {
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
        st.dirty |= st.show_machine;
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
        inline: false,
        setup: setup(cfg),
        rapid: rapid(cfg),
        file: String::new(),
    }));
    PLOT.with(|p| *p.borrow_mut() = Some(plot.clone()));
    let actions = ui.global::<Actions>();
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_resize(move |w, h| {
        let Some(ui) = weak.upgrade() else { return };
        let scale = ui.window().scale_factor();
        let mut st = p.borrow_mut();
        let size = ((w * scale) as u32, (h * scale) as u32);
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

/// Reload the toolpath when LinuxCNC reports a different program.
pub fn sync(ui: &MainWindow, file: &str) {
    with(|p| {
        if p.borrow().file == file {
            return;
        }
        p.borrow_mut().file = file.to_string();
        load(ui, p, file);
    });
}
