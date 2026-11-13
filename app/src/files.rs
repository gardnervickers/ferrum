//! Files screen: browse PROGRAM_PREFIX (no USB), preview a file, load it.
//! The op list, times and top-view preview arrive with the interpreter
//! backplot later in milestone 3.

use crate::config::Config;
use crate::{Actions, FileRow, Live, MainWindow};
use linuxcnc::{Command, Handle};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
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
        live.set_files(ModelRc::new(VecModel::from(rows)));
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

thread_local! {
    /// Bumped per selection, so a slow read of an earlier file is dropped.
    static SUMMARY_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Ops and times for the selected file, read in the background.
fn summarize(ui: &MainWindow, path: Option<PathBuf>) {
    let gen = SUMMARY_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let live = ui.global::<Live>();
    let Some(path) = path else {
        live.set_files_summary(SharedString::new());
        return;
    };
    live.set_files_summary("Reading the program…".into());
    let mut wired = false;
    let weak = ui.as_weak();
    crate::plot::with(|p| {
        wired = true;
        crate::plot::analyze(p, path, move |tp| {
            if SUMMARY_GEN.with(|g| g.get()) != gen {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                ui.global::<Live>().set_files_summary(crate::plot::op_summary(&tp).into());
            }
        });
    });
    if !wired {
        live.set_files_summary(SharedString::new());
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
    let (f, mc, weak) = (files.clone(), machine.clone(), ui.as_weak());
    actions.on_files_load(move || {
        let Some(path) = f.borrow().selected_path() else { return };
        mc.send(Command::ProgramOpen(path.to_string_lossy().into_owned()));
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
}
