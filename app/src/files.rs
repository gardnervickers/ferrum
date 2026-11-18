//! Files screen: browse PROGRAM_PREFIX (no USB), preview a file, load it.
//! The selected file is read through the interpreter in the background
//! (as the Run backplot does) for its top view, ops, tools and times.

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

struct Entry {
    name: String,
    dir: bool,
    modified: Option<SystemTime>,
    size: u64,
}

struct Files {
    root: PathBuf,
    cwd: PathBuf,
    entries: Vec<Entry>,
    selected: Option<usize>,
}

fn list(dir: &Path) -> Vec<Entry> {
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
                    if !dir && !ext.is_some_and(|x| EXTENSIONS.contains(&x.as_str())) {
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

fn num(v: f64) -> String {
    let s = format!("{v:.1}");
    let s = s.strip_suffix(".0").unwrap_or(&s);
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
    std::thread::spawn(move || {
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

pub fn wire(ui: &MainWindow, machine: &Handle, root: PathBuf) {
    let files = Rc::new(RefCell::new(Files { entries: list(&root), cwd: root.clone(), root, selected: None }));
    let live = ui.global::<Live>();
    files.borrow().show(&live);
    let actions = ui.global::<Actions>();

    let (f, weak) = (files.clone(), ui.as_weak());
    actions.on_files_tap(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let mut st = f.borrow_mut();
        let Some(e) = st.entries.get(i as usize) else { return };
        if e.dir {
            let next = st.cwd.join(&e.name);
            st.cwd = next;
            st.entries = list(&st.cwd);
            st.selected = None;
        } else {
            st.selected = Some(i as usize);
        }
        st.show(&ui.global::<Live>());
        let path = st.selected_path();
        drop(st);
        summarize(&ui, path);
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
        st.entries = list(&st.cwd);
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
        st.entries = list(&st.cwd);
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
        let Some(path) = f.borrow().selected_path() else { return };
        mc.send(Command::ProgramOpen(path));
        if let Some(ui) = weak.upgrade() {
            let live = ui.global::<Live>();
            live.set_run_tab("program".into());
            live.set_screen("run".into());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_gcode_and_folders_only() {
        let d = std::env::temp_dir().join(format!("millgui-files-{}", std::process::id()));
        std::fs::create_dir_all(d.join("jobs")).unwrap();
        for n in ["a.ngc", "b.NC", "notes.txt", ".hidden.ngc"] {
            std::fs::write(d.join(n), "G0 X0\n").unwrap();
        }
        let names: Vec<String> = list(&d).into_iter().map(|e| e.name).collect();
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
