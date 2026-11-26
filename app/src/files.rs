//! Files screen: browse PROGRAM_PREFIX and any USB drive, preview a file,
//! load it. The selected file is read through the interpreter in the
//! background (as the Run backplot does) for its top view, ops, tools and
//! times. A program on a USB drive is copied to PROGRAM_PREFIX/usb/ when
//! loaded, so the run never depends on the drive. A file with an
//! `[FILTER]PROGRAM_EXTENSION` extension is made into G-code by its filter
//! program when loaded, as in AXIS.

use crate::config::Config;
use crate::{Actions, FileOp, FileRow, Live, MainWindow};
use machine::{Command, Handle};
use preview::Toolpath;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, SharedString};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

const EXTENSIONS: &[&str] = &["ngc", "nc", "tap", "gcode"];
const PREVIEW_LINES: usize = 400;

/// The INI's `[FILTER]`: extra extensions to list, and the program that
/// turns each into G-code (none: the file is G-code already).
#[derive(Clone, Debug, Default)]
pub struct Filters {
    exts: Vec<String>,
    progs: Vec<(String, String)>,
}

impl Filters {
    /// `PROGRAM_EXTENSION = .png,.gif,.jpg Greyscale Depth Image` lines
    /// name extensions; `png = image-to-gcode` names a filter for one.
    pub fn from_cfg(cfg: &Config) -> Self {
        let mut f = Filters::default();
        for line in cfg.get_all("FILTER", "PROGRAM_EXTENSION") {
            let list = line.split_whitespace().next().unwrap_or("");
            for e in list.split(',') {
                let e = e.trim().trim_start_matches('*').trim_start_matches('.').to_ascii_lowercase();
                if e.is_empty() || f.exts.contains(&e) {
                    continue;
                }
                if let Some(p) = cfg.get("FILTER", &e).map(str::trim).filter(|p| !p.is_empty()) {
                    f.progs.push((e.clone(), p.to_string()));
                }
                f.exts.push(e);
            }
        }
        f
    }

    fn lists(&self, ext: &str) -> bool {
        EXTENSIONS.contains(&ext) || self.exts.iter().any(|e| e == ext)
    }

    /// The filter program for this file, if it needs one.
    fn program(&self, path: &Path) -> Option<&str> {
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        self.progs.iter().find(|(e, _)| *e == ext).map(|(_, p)| p.as_str())
    }
}

/// USB drives and other removable media: the mount points under /media
/// and /run/media (what the desktop's automounter uses), from /proc/mounts.
/// `FERRUM_TEST_DRIVES` (paths joined by `:`) stands in for them in tests
/// and screenshots.
fn drives() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("FERRUM_TEST_DRIVES") {
        return std::env::split_paths(&v).filter(|p| p.is_dir()).collect();
    }
    std::fs::read_to_string("/proc/mounts").map(|t| parse_mounts(&t)).unwrap_or_default()
}

fn parse_mounts(text: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = text
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        // /proc/mounts writes a space as \040.
        .map(|m| m.replace("\\040", " ").replace("\\011", "\t"))
        .filter(|m| (m.starts_with("/media/") || m.starts_with("/run/media/")) && m.matches('/').count() >= 2)
        .map(PathBuf::from)
        .collect();
    v.sort();
    v.dedup();
    v
}

fn place_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_uppercase()).unwrap_or_else(|| "USB".into())
}

struct Entry {
    name: String,
    dir: bool,
    modified: Option<SystemTime>,
    size: u64,
}

struct Files {
    /// PROGRAM_PREFIX.
    programs: PathBuf,
    /// The drives plugged in, and which place is open (0: PROGRAMS, n: drive n-1).
    drives: Vec<PathBuf>,
    place: usize,
    root: PathBuf,
    cwd: PathBuf,
    entries: Vec<Entry>,
    selected: Option<usize>,
    filters: Filters,
}

fn list(dir: &Path, filters: &Filters) -> Vec<Entry> {
    let mut out: Vec<Entry> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        return None;
                    }
                    let meta = e.metadata().ok()?;
                    let dir = meta.is_dir();
                    let ext = Path::new(&name).extension().map(|x| x.to_string_lossy().to_ascii_lowercase());
                    if !dir && !ext.is_some_and(|x| filters.lists(&x)) {
                        return None;
                    }
                    Some(Entry { name, dir, modified: meta.modified().ok(), size: meta.len() })
                })
                .collect()
        })
        .unwrap_or_default();
    // Folders first, then newest first: the file just posted is on top.
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then(b.modified.cmp(&a.modified)).then(a.name.cmp(&b.name)));
    out
}

fn when(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return String::new() };
    let t: chrono::DateTime<chrono::Local> = t.into();
    let now = chrono::Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("Today %H:%M").to_string()
    } else {
        t.format("%b %-d %H:%M").to_string()
    }
}

fn size(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

impl Files {
    fn open_place(&mut self, place: usize) {
        self.place = place.min(self.drives.len());
        self.root = if self.place == 0 { self.programs.clone() } else { self.drives[self.place - 1].clone() };
        self.cwd = self.root.clone();
        self.entries = list(&self.cwd, &self.filters);
        self.selected = None;
    }

    fn on_drive(&self) -> bool {
        self.place > 0
    }

    fn rel(&self) -> String {
        let r = self.cwd.strip_prefix(&self.root).unwrap_or(&self.cwd);
        let base = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        format!("{base}/{}", r.to_string_lossy())
    }

    fn show(&self, live: &Live) {
        let rows: Vec<FileRow> = self
            .entries
            .iter()
            .map(|e| FileRow {
                name: if e.dir { format!("{}/", e.name) } else { e.name.clone() }.into(),
                dir: e.dir,
                modified: when(e.modified).into(),
                size: if e.dir { SharedString::new() } else { size(e.size).into() },
            })
            .collect();
        crate::models::sync(live.get_files(), rows, |m| live.set_files(m));
        live.set_files_path(self.rel().into());
        live.set_files_selected(self.selected.map_or(-1, |i| i as i32));
        live.set_files_can_up(self.cwd != self.root);
        let mut places: Vec<SharedString> = vec!["PROGRAMS".into()];
        places.extend(self.drives.iter().map(|d| SharedString::from(format!("USB {}", place_name(d)))));
        crate::models::sync(live.get_files_places(), places, |m| live.set_files_places(m));
        live.set_files_place(self.place as i32);
        let sel = self.selected.and_then(|i| self.entries.get(i)).filter(|e| !e.dir);
        live.set_files_name(sel.map(|e| e.name.to_uppercase()).unwrap_or_default().into());
        let preview = sel.map(|e| head(&self.cwd.join(&e.name))).unwrap_or_default();
        live.set_files_preview(preview.into());
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected.and_then(|i| self.entries.get(i)).filter(|e| !e.dir).map(|e| self.cwd.join(&e.name))
    }
}

fn head(path: &Path) -> String {
    use std::io::{BufRead, BufReader};
    let Ok(f) = std::fs::File::open(path) else { return "Cannot read this file.".into() };
    BufReader::new(f).lines().take(PREVIEW_LINES).map_while(Result::ok).collect::<Vec<_>>().join("\n")
}

/// PROGRAM_PREFIX from [DISPLAY], relative to the INI's folder.
pub fn root(cfg: &Config) -> PathBuf {
    let p = cfg.get("DISPLAY", "PROGRAM_PREFIX").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("nc_files"));
    let p = if let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("~/")) {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest)
    } else {
        p
    };
    if p.is_absolute() { p } else { cfg.dir().join(p) }
}

/// The selected program's picture: what it shows and at what size.
#[derive(Default)]
struct Picture {
    tp: Option<Arc<Toolpath>>,
    /// The op picked out, if any.
    op: Option<usize>,
    /// The well's size in physical pixels, and the window's scale.
    size: (u32, u32),
    scale: f32,
    /// Bumped per selection, so a slow read of an earlier file is dropped.
    read: u64,
    /// Bumped per drawing, so only the latest one lands.
    drawn: u64,
}

thread_local! {
    static PICTURE: RefCell<Picture> = RefCell::new(Picture::default());
}

/// One decimal in mm, three in inches, trailing zeros dropped.
fn num(v: f64) -> String {
    let places = if crate::machine_info::get().units.inch() { 3 } else { 1 };
    let s = format!("{v:.*}", places);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { &s };
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// "5 OPS · ~14 MIN · T14 T23": ops, estimated time, the tools in order of use.
fn summary_line(tp: &Toolpath) -> String {
    if tp.segs.is_empty() {
        return String::new();
    }
    let n = tp.ops.len();
    let mut tools: Vec<i32> = Vec::new();
    for o in &tp.ops {
        if o.tool > 0 && !tools.contains(&o.tool) {
            tools.push(o.tool);
        }
    }
    let mut out = format!("{} OP{} · ~{}", n, if n == 1 { "" } else { "S" }, crate::plot::fmt_time(tp.seconds()).to_uppercase());
    if !tools.is_empty() {
        out += " ·";
        for t in tools {
            out += &format!(" T{t}");
        }
    }
    out
}

/// Name, tool and estimated time of each op.
fn op_rows(tp: &Toolpath) -> Vec<FileOp> {
    tp.ops
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let tag = format!("T{}", o.tool);
            // JOB names end in their tool, which has its own column here.
            let name = match o.name.strip_suffix(&tag) {
                Some(rest) if o.tool > 0 && rest.ends_with(' ') => rest.trim_end(),
                _ => o.name.as_str(),
            };
            FileOp {
                name: if name.is_empty() { format!("OP {}", i + 1) } else { name.to_string() }.into(),
                tool: if o.tool > 0 { tag } else { String::new() }.into(),
                time: if o.seconds > 0.0 { format!("~{}", crate::plot::fmt_time(o.seconds)) } else { String::new() }.into(),
                color: i as i32,
            }
        })
        .collect()
}

/// Where the cuts sit in work coordinates.
fn extent(tp: &Toolpath) -> String {
    match tp.bounds() {
        Some((lo, hi)) if !tp.segs.is_empty() => {
            let xyz = format!("X {} to {}  ·  Y {} to {}  ·  lowest Z {}", num(lo[0]), num(hi[0]), num(lo[1]), num(hi[1]), num(lo[2]));
            // A program on the rotary (drawn wrapped around it): how far A turns.
            if tp.a_wrap == 0.0 {
                return xyz;
            }
            let span = hi[3] - lo[3];
            if span > 360.0 {
                format!("{xyz}  ·  A turns {} times", num((span / 36.0).round() / 10.0))
            } else {
                // A wrapped rotary counts on past 360: show where on the dial.
                let base = (lo[3] / 360.0).floor() * 360.0;
                format!("{xyz}  ·  A {} to {}°", num(lo[3] - base), num(hi[3] - base))
            }
        }
        _ => String::new(),
    }
}

/// Draw the picture for the current program, op and size. The drawing
/// takes as long as the program is big, so it happens off the UI thread.
fn draw(ui: &MainWindow) {
    let (tp, op, (w, h), scale, id) = PICTURE.with(|p| {
        let mut p = p.borrow_mut();
        p.drawn += 1;
        (p.tp.clone(), p.op, p.size, p.scale, p.drawn)
    });
    let Some(tp) = tp else { return };
    if tp.segs.is_empty() || w < 16 || h < 16 {
        return;
    }
    let margin = 36.0 * scale.max(0.25) as f64;
    let land = move |ui: &MainWindow, pm: preview::Pixmap, zero: (f32, f32)| {
        if PICTURE.with(|p| p.borrow().drawn) != id {
            return;
        }
        let live = ui.global::<Live>();
        live.set_files_plot(Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(pm.data(), pm.width(), pm.height())));
        live.set_files_zero_x(zero.0 / pm.width() as f32);
        live.set_files_zero_y(zero.1 / pm.height() as f32);
    };
    let mut inline = false;
    crate::plot::with(|p| inline = crate::plot::inline(p));
    if inline {
        let (pm, zero) = preview::software::top_view(&tp, w, h, op, margin);
        land(ui, pm, zero);
        return;
    }
    let weak = ui.as_weak();
    crate::util::background(move || {
        let (pm, zero) = preview::software::top_view(&tp, w, h, op, margin);
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                land(&ui, pm, zero);
            }
        });
    });
}

/// Nothing to show for the selection yet (or at all).
fn clear(live: &Live, status: &str) {
    live.set_files_summary(SharedString::new());
    crate::models::sync(live.get_files_ops(), Vec::<FileOp>::new(), |m| live.set_files_ops(m));
    live.set_files_op(-1);
    live.set_files_extent(SharedString::new());
    live.set_files_warning(SharedString::new());
    live.set_files_plot(Image::default());
    live.set_files_zero_x(-1.0);
    live.set_files_status(status.into());
}

/// The picture, ops and times for the selected file, read in the background.
fn summarize(ui: &MainWindow, path: Option<PathBuf>) {
    let read = PICTURE.with(|p| {
        let mut p = p.borrow_mut();
        p.read += 1;
        p.drawn += 1;
        p.tp = None;
        p.op = None;
        p.read
    });
    let live = ui.global::<Live>();
    let Some(path) = path else {
        clear(&live, "");
        return;
    };
    clear(&live, "Reading the program…");
    let mut wired = false;
    let weak = ui.as_weak();
    crate::plot::with(|p| {
        wired = true;
        crate::plot::analyze(p, path, move |tp| {
            if PICTURE.with(|p| p.borrow().read) != read {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            let live = ui.global::<Live>();
            live.set_files_summary(summary_line(&tp).into());
            crate::models::sync(live.get_files_ops(), op_rows(&tp), |m| live.set_files_ops(m));
            live.set_files_extent(extent(&tp).into());
            live.set_files_status(
                match &tp.error {
                    Some(e) if tp.segs.is_empty() => format!("Cannot read this program: {e}"),
                    _ if tp.segs.is_empty() => "This program has no moves.".into(),
                    _ => String::new(),
                }
                .into(),
            );
            let partial = tp.error.as_ref().filter(|_| !tp.segs.is_empty());
            live.set_files_warning(partial.map(|e| format!("Stops early: {e}")).unwrap_or_default().into());
            PICTURE.with(|p| p.borrow_mut().tp = Some(Arc::new(tp)));
            draw(&ui);
        });
    });
    if !wired {
        clear(&live, "");
    }
}

pub fn wire(ui: &MainWindow, machine: &Handle, root: PathBuf, filters: Filters) -> slint::Timer {
    let files = Rc::new(RefCell::new(Files {
        entries: list(&root, &filters),
        programs: root.clone(),
        drives: drives(),
        place: 0,
        cwd: root.clone(),
        root,
        selected: None,
        filters,
    }));
    let live = ui.global::<Live>();
    files.borrow().show(&live);
    let actions = ui.global::<Actions>();

    // A drive plugged in shows up as a place; one pulled out closes.
    let watch = slint::Timer::default();
    let (f, weak) = (files.clone(), ui.as_weak());
    watch.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(2), move || {
        let now = drives();
        let mut st = f.borrow_mut();
        if now == st.drives {
            return;
        }
        let Some(ui) = weak.upgrade() else { return };
        let open = st.on_drive().then(|| st.root.clone());
        let added: Vec<PathBuf> = now.iter().filter(|d| !st.drives.contains(d)).cloned().collect();
        st.drives = now;
        match open {
            Some(r) => match st.drives.iter().position(|d| *d == r) {
                Some(i) => st.place = i + 1,
                None => {
                    st.open_place(0);
                    crate::toast(&ui, format!("USB {} was removed.", place_name(&r)));
                    st.show(&ui.global::<Live>());
                    drop(st);
                    summarize(&ui, None);
                    return;
                }
            },
            None => {}
        }
        // A new drive opens by itself while FILES is on screen.
        if let Some(d) = added.first() {
            if ui.global::<Live>().get_screen() == "files" && !st.on_drive() {
                if let Some(i) = st.drives.iter().position(|x| x == d) {
                    st.open_place(i + 1);
                    st.show(&ui.global::<Live>());
                    drop(st);
                    summarize(&ui, None);
                    return;
                }
            }
        }
        st.show(&ui.global::<Live>());
    });
    let (f, weak) = (files.clone(), ui.as_weak());
    actions.on_files_place(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = f.borrow_mut();
        st.open_place(i.max(0) as usize);
        st.show(&ui.global::<Live>());
        drop(st);
        summarize(&ui, None);
    });

    let (f, weak) = (files.clone(), ui.as_weak());
    actions.on_files_tap(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = f.borrow_mut();
        let Some(e) = st.entries.get(i as usize) else { return };
        if e.dir {
            let next = st.cwd.join(&e.name);
            st.cwd = next;
            st.entries = list(&st.cwd, &st.filters);
            st.selected = None;
        } else {
            st.selected = Some(i as usize);
        }
        st.show(&ui.global::<Live>());
        let path = st.selected_path();
        let filter = path.as_deref().and_then(|p| st.filters.program(p)).map(str::to_string);
        drop(st);
        match filter {
            Some(prog) => {
                summarize(&ui, None);
                clear(&ui.global::<Live>(), &format!("Made into G-code by {prog} when it is loaded."));
            }
            None => summarize(&ui, path),
        }
    });
    let (f, weak) = (files.clone(), ui.as_weak());
    actions.on_files_up(move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = f.borrow_mut();
        if st.cwd != st.root {
            if let Some(p) = st.cwd.parent().map(Path::to_path_buf) {
                st.cwd = p;
            }
        }
        st.entries = list(&st.cwd, &st.filters);
        st.selected = None;
        st.show(&ui.global::<Live>());
        drop(st);
        summarize(&ui, None);
    });
    let (f, weak) = (files.clone(), ui.as_weak());
    actions.on_files_refresh(move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = f.borrow_mut();
        let keep = st.selected.and_then(|i| st.entries.get(i)).map(|e| e.name.clone());
        st.entries = list(&st.cwd, &st.filters);
        st.selected = keep.and_then(|n| st.entries.iter().position(|e| e.name == n));
        st.show(&ui.global::<Live>());
        let path = st.selected_path();
        drop(st);
        summarize(&ui, path);
    });
    // A tap on an op picks it out in the picture; again shows them all.
    let weak = ui.as_weak();
    actions.on_files_op_tap(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let op = PICTURE.with(|p| {
            let mut p = p.borrow_mut();
            let n = p.tp.as_ref().map_or(0, |tp| tp.ops.len());
            p.op = if i < 0 || i as usize >= n || p.op == Some(i as usize) { None } else { Some(i as usize) };
            p.op
        });
        ui.global::<Live>().set_files_op(op.map_or(-1, |o| o as i32));
        draw(&ui);
    });
    let weak = ui.as_weak();
    actions.on_files_plot_resize(move |w, h| {
        let Some(ui) = weak.upgrade() else { return };
        let scale = ui.window().scale_factor();
        let size = (((w * scale) as u32).min(4096), ((h * scale) as u32).min(4096));
        let changed = PICTURE.with(|p| {
            let mut p = p.borrow_mut();
            let changed = p.size != size;
            p.size = size;
            p.scale = scale;
            changed
        });
        if changed {
            draw(&ui);
        }
    });
    let (f, mc, weak) = (files.clone(), machine.clone(), ui.as_weak());
    actions.on_files_load(move || {
        let st = f.borrow();
        let Some(mut path) = st.selected_path() else { return };
        let Some(ui) = weak.upgrade() else { return };
        // Off a USB drive: run a copy, so pulling the drive cannot stop the run.
        if st.on_drive() {
            match copy_in(&path, &st.programs) {
                Ok(p) => path = p,
                Err(e) => {
                    crate::toast(&ui, format!("Cannot copy {} from the USB drive: {e}", path.display()));
                    return;
                }
            }
        }
        let filter = st.filters.program(&path).map(str::to_string);
        drop(st);
        let open = {
            let (mc, weak) = (mc.clone(), weak.clone());
            move |path: PathBuf| {
                mc.send(Command::ProgramOpen(path));
                if let Some(ui) = weak.upgrade() {
                    let live = ui.global::<Live>();
                    live.set_run_tab("program".into());
                    live.set_screen("run".into());
                }
            }
        };
        let Some(prog) = filter else {
            open(path);
            return;
        };
        ui.global::<Live>().set_files_status(format!("Making G-code with {prog}…").into());
        let weak = weak.clone();
        std::thread::spawn(move || {
            let res = run_filter(&prog, &path, &crate::job::cache_dir().join("filtered"));
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else { return };
                match res {
                    Ok(out) => open(out),
                    Err(e) => {
                        ui.global::<Live>().set_files_status(SharedString::new());
                        crate::toast_until_tapped(&ui, format!("{prog} could not convert {}: {e}", path.display()));
                    }
                }
            });
        });
    });
    watch
}

/// Copy a program off a USB drive into PROGRAM_PREFIX/usb/ (a copy of the
/// same name there is replaced).
fn copy_in(path: &Path, programs: &Path) -> Result<PathBuf, String> {
    let dir = programs.join("usb");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let to = dir.join(path.file_name().ok_or("no file name")?);
    std::fs::copy(path, &to).map_err(|e| e.to_string())?;
    Ok(to)
}

/// Run an `[FILTER]` program as AXIS does: `program file`, G-code on its
/// standard output. The G-code goes to ~/.cache/ferrum/filtered/.
fn run_filter(prog: &str, path: &Path, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "program".into());
    let out = dir.join(format!("{stem}.ngc"));
    let file = std::fs::File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let res = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{prog} \"$0\""))
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(file)
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    if !res.status.success() {
        let err = String::from_utf8_lossy(&res.stderr);
        let last = err.lines().filter(|l| !l.starts_with("FILTER_PROGRESS=")).last().unwrap_or("").trim().to_string();
        return Err(if last.is_empty() { format!("it exited with {}", res.status) } else { last });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_drives_from_mounts() {
        let m = "/dev/mmcblk0p2 / ext4 rw 0 0\n/dev/sda1 /media/cnc/MY\\040STICK vfat rw 0 0\n/dev/sdb1 /run/media/cnc/PARTS vfat rw 0 0\ntmpfs /run tmpfs rw 0 0\n/dev/sdc1 /media vfat rw 0 0\n";
        assert_eq!(parse_mounts(m), vec![PathBuf::from("/media/cnc/MY STICK"), PathBuf::from("/run/media/cnc/PARTS")]);
    }

    #[test]
    fn filters_from_the_ini() {
        let mut cfg = Config::default();
        cfg.insert("FILTER", "PROGRAM_EXTENSION", ".png,.gif,.jpg Greyscale Depth Image");
        cfg.insert("FILTER", "PROGRAM_EXTENSION", ".py Python Script");
        cfg.insert("FILTER", "PROGRAM_EXTENSION", ".ngc,.nc G-code");
        cfg.insert("FILTER", "png", "image-to-gcode");
        cfg.insert("FILTER", "py", "python3");
        let f = Filters::from_cfg(&cfg);
        assert!(f.lists("gif") && f.lists("py") && f.lists("ngc") && !f.lists("txt"));
        assert_eq!(f.program(Path::new("/x/a.PNG")), Some("image-to-gcode"));
        assert_eq!(f.program(Path::new("/x/a.gif")), None);
        assert_eq!(f.program(Path::new("/x/a.ngc")), None);
    }

    #[test]
    fn a_filter_writes_gcode() {
        let d = std::env::temp_dir().join(format!("ferrum-filter-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let src = d.join("part one.txt");
        std::fs::write(&src, "G0 X1\n").unwrap();
        let out = run_filter("cat", &src, &d.join("out")).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "G0 X1\n");
        assert!(run_filter("false", &src, &d.join("out")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lists_gcode_and_folders_only() {
        let d = std::env::temp_dir().join(format!("ferrum-files-{}", std::process::id()));
        std::fs::create_dir_all(d.join("jobs")).unwrap();
        for n in ["a.ngc", "b.NC", "notes.txt", ".hidden.ngc"] {
            std::fs::write(d.join(n), "G0 X0\n").unwrap();
        }
        let names: Vec<String> = list(&d, &Filters::default()).into_iter().map(|e| e.name).collect();
        assert_eq!(names[0], "jobs");
        assert!(names.contains(&"a.ngc".into()) && names.contains(&"b.NC".into()));
        assert!(!names.iter().any(|n| n == "notes.txt" || n.starts_with('.')));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn summary_ops_and_extent() {
        let src = "(MYMILL-OP 1/2 ID=A FACE 100 X 60 T14)\nT14 M6\nG0 X-10 Y0 Z5\nG1 Z-0.5 F600\nX110\n\
                   (MYMILL-OP 2/2 ID=B DRILL1)\nT7 M6\nG0 X20 Y30 Z5\nG1 Z-6 F100\nG0 Z5\n";
        let tp = preview::mini::parse(src, 100.0);
        assert!(summary_line(&tp).starts_with("2 OPS · ~"), "{}", summary_line(&tp));
        assert!(summary_line(&tp).ends_with("· T14 T7"), "{}", summary_line(&tp));
        let rows = op_rows(&tp);
        let got: Vec<_> = rows.iter().map(|r| (r.name.as_str(), r.tool.as_str(), r.color)).collect();
        assert_eq!(got, [("FACE 100 X 60", "T14", 0), ("DRILL1", "T7", 1)]);
        assert!(rows[0].time.starts_with('~'));
        assert_eq!(extent(&tp), "X -10 to 110  ·  Y 0 to 30  ·  lowest Z -6");
        assert_eq!(summary_line(&Toolpath::default()), "");
        assert_eq!(extent(&Toolpath::default()), "");
    }
}
