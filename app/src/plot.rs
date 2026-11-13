//! Backplot on the Run screen: the loaded program's toolpath from the
//! interpreter (in the background), the live tool position and its trail.

use crate::config::Config;
use crate::{Actions, Live, MainWindow};
use preview::software::{Base, View};
use preview::{rs274, Toolpath, P4};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

const TRAIL_MAX: usize = 50_000;

/// What a base image was drawn for: toolpath id, view, size.
type Key = (usize, View, u32, u32);

pub struct Plot {
    tp: Arc<Toolpath>,
    id: usize,
    view: View,
    size: (u32, u32),
    trail: Vec<P4>,
    tool: Option<P4>,
    dirty: bool,
    /// The drawn toolpath, and the one being drawn on a worker thread. The
    /// last base stays on screen until its replacement is ready.
    base: Option<(Key, Base)>,
    pending: Option<Key>,
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
        let key = (self.id, self.view, w, h);
        if self.base.as_ref().map(|b| b.0) != Some(key) {
            if self.inline {
                self.base = Some((key, Base::draw(&self.tp, self.view, w, h, None)));
            } else if self.pending != Some(key) {
                self.pending = Some(key);
                let tp = self.tp.clone();
                std::thread::spawn(move || {
                    let b = Base::draw(&tp, key.1, key.2, key.3, None);
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
        let Some((_, base)) = &self.base else { return };
        let pm = base.compose(&self.trail, self.tool);
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(pm.data(), pm.width(), pm.height());
        ui.global::<Live>().set_plot(Image::from_rgba8(buf));
    }

    fn set_toolpath(&mut self, ui: &MainWindow, tp: Toolpath) {
        let live = ui.global::<Live>();
        let secs = tp.seconds();
        let ops = tp.ops.len();
        live.set_plot_summary(if tp.segs.is_empty() {
            String::new()
        } else {
            format!("{} OPS · ~{}", ops, fmt_time(secs))
        }
        .into());
        if let Some(e) = &tp.error {
            live.set_message(format!("Preview: {e}").into());
            live.set_message_error(true);
        }
        self.tp = Arc::new(tp);
        self.id += 1;
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

/// Build the toolpath for `file` off the UI thread.
pub fn load(ui: &MainWindow, plot: &Rc<RefCell<Plot>>, file: &str) {
    let weak = ui.as_weak();
    let path = PathBuf::from(file);
    let (setup, rapid) = {
        let p = plot.borrow();
        (p.setup.as_ref().map(|s| rs274::Setup { ini: s.ini.clone(), config_dir: s.config_dir.clone(), var_file: s.var_file.clone(), tool_table: s.tool_table.clone(), rapid_mm_s: s.rapid_mm_s }), p.rapid)
    };
    {
        let mut p = plot.borrow_mut();
        p.trail.clear();
        p.set_toolpath(ui, Toolpath::default());
    }
    if file.is_empty() {
        return;
    }
    ui.global::<Live>().set_plot_summary("READING PROGRAM…".into());
    std::thread::spawn(move || {
        let tp = match &setup {
            Some(s) => rs274::run(s, &path),
            None => match std::fs::read_to_string(&path) {
                Ok(t) => preview::mini::parse(&t, rapid),
                Err(e) => Toolpath { error: Some(e.to_string()), ..Default::default() },
            },
        };
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                PLOT.with(|p| {
                    if let Some(p) = p.borrow().as_ref() {
                        p.borrow_mut().set_toolpath(&ui, tp);
                    }
                });
            }
        });
    });
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
        }
        st.trail.push(p);
    }
    st.tool = Some(p);
    st.dirty = true;
}

thread_local! {
    static PLOT: RefCell<Option<Rc<RefCell<Plot>>>> = const { RefCell::new(None) };
}

pub fn wire(ui: &MainWindow, cfg: &Config) -> Rc<RefCell<Plot>> {
    let plot = Rc::new(RefCell::new(Plot {
        tp: Arc::new(Toolpath::default()),
        id: 0,
        view: View::Iso,
        size: (0, 0),
        trail: Vec::new(),
        tool: None,
        dirty: false,
        base: None,
        pending: None,
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
        ui.global::<Live>().set_plot_view(v);
        st.redraw(&ui);
    });
    let (p, weak) = (plot.clone(), ui.as_weak());
    actions.on_plot_clear_trail(move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = p.borrow_mut();
        st.trail.clear();
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
