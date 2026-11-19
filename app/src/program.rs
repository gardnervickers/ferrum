//! The loaded program's G-code for the Run screen. Reloaded when LinuxCNC
//! reports a different file (loads from FILES, the pendant or a remap) or
//! the file was written again.

use crate::{Actions, Live, MainWindow};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;

/// What tells one loaded program from another: its path and, so that a
/// file written again under the same name (a re-post from CAM, a rebuilt
/// job) is read again, its modification time and size.
pub type Stamp = (String, Option<std::time::SystemTime>, u64);

pub fn stamp(file: &str) -> Stamp {
    let meta = if file.is_empty() { None } else { std::fs::metadata(file).ok() };
    (file.to_string(), meta.as_ref().and_then(|m| m.modified().ok()), meta.map_or(0, |m| m.len()))
}

/// Whether the program shown as `shown` should be read again for `file`.
/// While something runs only a different path counts: the listing must
/// not change under a running program.
pub fn stale(shown: &Stamp, file: &str, running: bool) -> Option<Stamp> {
    if shown.0 == file && running {
        return None;
    }
    let now = stamp(file);
    (*shown != now).then_some(now)
}

thread_local! {
    static LOADED: RefCell<Stamp> = RefCell::new((String::new(), None, 0));
}

/// Lines beyond this are not shown (the program still runs in full).
const MAX_LINES: usize = 500_000;

pub fn sync(ui: &MainWindow, file: &str, running: bool) {
    let changed = LOADED.with(|l| {
        let mut l = l.borrow_mut();
        stale(&l, file, running).map(|now| *l = now).is_some()
    });
    if !changed {
        return;
    }
    let lines: Vec<SharedString> = if file.is_empty() {
        Vec::new()
    } else {
        match std::fs::read_to_string(file) {
            Ok(t) => t.lines().take(MAX_LINES).map(SharedString::from).collect(),
            Err(e) => vec![format!("(cannot read {file}: {e})").into()],
        }
    };
    let live = ui.global::<Live>();
    live.set_gcode_count(lines.len() as i32);
    live.set_gcode_lines(ModelRc::new(VecModel::from(lines)));
    live.set_gcode_selected(-1);
}

/// Index of the first line at or after `start` that calls M6.
pub fn next_m6<'a>(lines: impl Iterator<Item = &'a str>, start: usize) -> Option<usize> {
    lines.enumerate().skip(start).find(|(_, l)| {
        let code = l.split(['(', ';']).next().unwrap_or("").to_ascii_uppercase();
        code.split_whitespace().any(|w| w == "M6" || w == "M06") || code.contains("M6T") || code.contains("M06T")
    }).map(|(i, _)| i)
}

pub fn wire(ui: &MainWindow) {
    let actions = ui.global::<Actions>();
    let weak = ui.as_weak();
    actions.on_gcode_next_m6(move || {
        let Some(ui) = weak.upgrade() else { return };
        let live = ui.global::<Live>();
        let model = live.get_gcode_lines();
        // After the tapped line, or the executing line, whichever is later.
        let start = (live.get_gcode_selected().max(live.get_line() - 1) + 1).max(0) as usize;
        let lines: Vec<SharedString> = model.iter().collect();
        match next_m6(lines.iter().map(|s| s.as_str()), start) {
            Some(i) => live.set_gcode_selected(i as i32),
            None => {
                live.set_message("No M6 after this line".into());
                live.set_message_error(false);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_written_again_is_stale_unless_running() {
        let f = std::env::temp_dir().join(format!("ferrum-stamp-{}.ngc", std::process::id()));
        std::fs::write(&f, "G0 X0\n").unwrap();
        let name = f.to_string_lossy().into_owned();
        let shown = stamp(&name);
        assert_eq!(stale(&shown, &name, false), None);
        std::fs::write(&f, "G0 X0\nG0 X1\n").unwrap();
        assert_eq!(stale(&shown, &name, true), None);
        assert!(stale(&shown, &name, false).is_some());
        // Another program always counts.
        assert!(stale(&shown, "", true).is_some());
        std::fs::remove_file(f).unwrap();
    }

    #[test]
    fn finds_tool_changes_not_comments() {
        let g = ["G0 X0", "(M6 in a comment)", "T5 M6", "G1 X1", "M06T7", "M61 Q2"];
        assert_eq!(next_m6(g.iter().copied(), 0), Some(2));
        assert_eq!(next_m6(g.iter().copied(), 3), Some(4));
        assert_eq!(next_m6(g.iter().copied(), 5), None);
    }
}
