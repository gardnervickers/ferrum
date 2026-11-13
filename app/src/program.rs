//! The loaded program's G-code for the Run screen. Reloaded when LinuxCNC
//! reports a different file (loads from FILES, the pendant or a remap).

use crate::{Actions, Live, MainWindow};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;

thread_local! {
    static LOADED: RefCell<String> = RefCell::new(String::new());
}

/// Lines beyond this are not shown (the program still runs in full).
const MAX_LINES: usize = 500_000;

pub fn sync(ui: &MainWindow, file: &str) {
    let changed = LOADED.with(|l| {
        let mut l = l.borrow_mut();
        (*l != file).then(|| *l = file.to_string()).is_some()
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
    fn finds_tool_changes_not_comments() {
        let g = ["G0 X0", "(M6 in a comment)", "T5 M6", "G1 X1", "M06T7", "M61 Q2"];
        assert_eq!(next_m6(g.iter().copied(), 0), Some(2));
        assert_eq!(next_m6(g.iter().copied(), 3), Some(4));
        assert_eq!(next_m6(g.iter().copied(), 5), None);
    }
}
