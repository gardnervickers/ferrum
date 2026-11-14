//! Offsets timeline: every change to a work offset or a tool's length and
//! diameter that millgui makes or sees, with the values before and after,
//! and a way to put the earlier values back.
//!
//! The log only observes. LinuxCNC stays the owner of the offsets and the
//! tool table: a restore is an ordinary edit (G10 L2 for a work system, a
//! table write for a tool) made by the owning screen, and is itself logged,
//! so a restore can be undone the same way.
//!
//! What it cannot see: a change made while millgui is not running, and a
//! change a program makes to a work system that is not active (OFFSETS
//! learns of that one when the system becomes active, and it is logged
//! then, marked as found late).

use crate::{Actions, HistoryRow, Live, MainWindow};
use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;

/// Entries kept; the file is trimmed to this when it is read.
const MAX: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Target {
    /// Work system index, 0 = G54. Values are X Y Z A and the XY rotation.
    Wcs(usize),
    /// Tool number. Values are length Z and diameter.
    Tool(i32),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    /// Unix seconds.
    pub time: i64,
    pub target: Target,
    pub old: Vec<f64>,
    pub new: Vec<f64>,
    pub cause: String,
    /// Tools: the wear adjustment (length, diameter) in effect after this
    /// change, kept so a restore can reinstate the one from before. What
    /// is in effect now lives in the wear store below, not here.
    #[serde(default)]
    pub wear: [f64; 2],
}

#[derive(Default)]
struct History {
    path: Option<PathBuf>,
    entries: Vec<Entry>,
    selected: Option<u64>,
    ui: Option<slint::Weak<MainWindow>>,
}

/// Wear adjustments in effect, per tool and per value (0 length, 1
/// diameter): the adjustment and the table value it was made to. It
/// counts only while the table still holds that value, so a touch off, a
/// hand edit of the file or a change made while millgui was closed ends it
/// without anyone having to notice the change.
#[derive(Default)]
struct WearStore {
    path: Option<PathBuf>,
    tools: std::collections::BTreeMap<i32, [Option<(f64, f64)>; 2]>,
}

impl WearStore {
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let tmp = path.with_extension("tmp");
        let text = serde_json::to_string(&self.tools).unwrap_or_default();
        let ok = path.parent().map_or(Ok(()), std::fs::create_dir_all).is_ok() && std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).is_ok();
        if !ok {
            eprintln!("millgui: could not save {}", path.display());
        }
    }
}

thread_local! {
    static HISTORY: RefCell<History> = RefCell::new(History::default());
    static WEAR: RefCell<WearStore> = RefCell::new(WearStore::default());
}

/// The wear adjustment (length, diameter) in effect for a tool whose table
/// values are `now`.
pub fn wear(tool: i32, now: [f64; 2]) -> [f64; 2] {
    WEAR.with(|w| {
        let w = w.borrow();
        let slots = w.tools.get(&tool).copied().unwrap_or_default();
        [0, 1].map(|i| slots[i].filter(|(_, at)| (at - now[i]).abs() <= EPS).map_or(0.0, |(v, _)| v))
    })
}

/// Set a tool's wear adjustment on one value (0 length, 1 diameter), made
/// to the table value `at`. Zero wear forgets it.
pub fn set_wear(tool: i32, axis: usize, wear: f64, at: f64) {
    WEAR.with(|w| {
        let mut w = w.borrow_mut();
        let slots = w.tools.entry(tool).or_default();
        slots[axis] = (wear.abs() > EPS).then_some((wear, at));
        if slots.iter().all(Option::is_none) {
            w.tools.remove(&tool);
        }
        w.save();
    });
}

/// A tool left the table.
pub fn forget_wear(tool: i32) {
    WEAR.with(|w| {
        let mut w = w.borrow_mut();
        if w.tools.remove(&tool).is_some() {
            w.save();
        }
    });
}

const WCS: [&str; 9] = ["G54", "G55", "G56", "G57", "G58", "G59", "G59.1", "G59.2", "G59.3"];
const WCS_COLS: [&str; 5] = ["X", "Y", "Z", "A", "ROT"];
const TOOL_COLS: [&str; 2] = ["LENGTH", "DIA"];

/// Differences smaller than this are not changes (status round-off).
pub const EPS: f64 = 5e-5;

pub fn differs(a: &[f64], b: &[f64]) -> bool {
    a.len() != b.len() || a.iter().zip(b).any(|(x, y)| (x - y).abs() > EPS)
}

impl Entry {
    pub fn name(&self) -> String {
        match self.target {
            Target::Wcs(i) => WCS.get(i).copied().unwrap_or("WCS").to_string(),
            Target::Tool(t) => format!("T{t}"),
        }
    }

    /// The values that changed, each as "X 1.000 → 2.000".
    fn change(&self) -> String {
        let cols: &[&str] = match self.target {
            Target::Wcs(_) => &WCS_COLS,
            Target::Tool(_) => &TOOL_COLS,
        };
        let parts: Vec<String> = cols
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let (a, b) = (*self.old.get(i)?, *self.new.get(i)?);
                ((a - b).abs() > EPS).then(|| format!("{c} {a:.3} → {b:.3}"))
            })
            .collect();
        parts.join("   ")
    }

    /// The columns this entry changed, with the values from before it.
    pub fn before(&self) -> Vec<(usize, f64)> {
        self.old.iter().zip(&self.new).enumerate().filter(|(_, (a, b))| (*a - *b).abs() > EPS).map(|(i, (a, _))| (i, *a)).collect()
    }

    /// What a restore of this entry writes: "G54 X 120.000, Z -212.400".
    fn restores(&self) -> String {
        let cols: &[&str] = match self.target {
            Target::Wcs(_) => &WCS_COLS,
            Target::Tool(_) => &TOOL_COLS,
        };
        let vals: Vec<String> = self.before().iter().map(|(i, v)| format!("{} {v:.3}", cols[*i])).collect();
        format!("{} {}", self.name(), vals.join(", "))
    }

    fn when(&self, now: i64) -> String {
        let Some(t) = Local.timestamp_opt(self.time, 0).single() else { return String::new() };
        let today = Local.timestamp_opt(now, 0).single().is_some_and(|n| n.date_naive() == t.date_naive());
        if today { t.format("%H:%M:%S").to_string() } else { t.format("%b %-d  %H:%M").to_string() }
    }
}

fn now() -> i64 {
    Local::now().timestamp()
}

impl History {
    fn load(&mut self) {
        let Some(text) = self.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) else { return };
        let mut entries: Vec<Entry> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        // A line cut short by a power loss would swallow the next entry
        // appended to it: rewrite the file from what parsed.
        let torn = !text.is_empty() && !text.ends_with('\n');
        let long = entries.len() > MAX;
        if long {
            entries.drain(..entries.len() - MAX);
        }
        self.entries = entries;
        if torn || long {
            self.rewrite();
        }
    }

    fn rewrite(&self) {
        let Some(path) = &self.path else { return };
        let text: String = self.entries.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
            eprintln!("millgui: could not save {}", path.display());
        }
    }

    fn append(&self, e: &Entry) {
        let Some(path) = &self.path else { return };
        let line = serde_json::to_string(e).unwrap_or_default();
        let ok = path.parent().map_or(Ok(()), std::fs::create_dir_all).is_ok()
            && std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| writeln!(f, "{line}")).is_ok();
        if !ok {
            eprintln!("millgui: could not save {}", path.display());
        }
    }

    fn show(&self) {
        let Some(ui) = self.ui.as_ref().and_then(|u| u.upgrade()) else { return };
        let live = ui.global::<Live>();
        let now = now();
        // Newest first.
        let rows: Vec<HistoryRow> = self
            .entries
            .iter()
            .rev()
            .map(|e| HistoryRow {
                when: e.when(now).into(),
                what: e.name().into(),
                tool: matches!(e.target, Target::Tool(_)),
                change: e.change().into(),
                cause: e.cause.as_str().into(),
            })
            .collect();
        live.set_history(ModelRc::new(VecModel::from(rows)));
        let sel = self.selected.and_then(|id| self.entries.iter().rev().position(|e| e.id == id));
        live.set_history_selected(sel.map_or(-1, |i| i as i32));
        let picked = self.selected.and_then(|id| self.entries.iter().find(|e| e.id == id));
        live.set_history_restore_label(picked.map_or_else(|| "RESTORE".into(), |e| format!("RESTORE {}", e.name())).into());
        // Say exactly what will be written.
        let note = match picked {
            Some(e) => format!("Sets {}: the values from before this change. Other values are left alone. It is listed as a change itself.", e.restores()),
            None => "Select a change to put its earlier values back.".into(),
        };
        live.set_history_restore_note(note.into());
    }
}

/// Log a change. `old` and `new` are the target's whole row; a call where
/// they do not differ is ignored. For tools, `wear` is the adjustment in
/// effect afterwards.
pub fn record(target: Target, old: &[f64], new: &[f64], cause: &str, wear: [f64; 2]) {
    if !differs(old, new) {
        return;
    }
    HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        let id = h.entries.last().map_or(1, |e| e.id + 1);
        let e = Entry { id, time: now(), target, old: old.to_vec(), new: new.to_vec(), cause: cause.into(), wear };
        h.append(&e);
        h.entries.push(e);
        if h.entries.len() > MAX + 50 {
            let n = h.entries.len() - MAX;
            h.entries.drain(..n);
            h.rewrite();
        }
        h.show();
    });
}

/// The wear adjustment in effect just before an entry was made.
fn wear_before(entries: &[Entry], e: &Entry) -> [f64; 2] {
    entries.iter().rev().find(|x| x.target == e.target && x.id < e.id).map_or([0.0; 2], |x| x.wear)
}

/// Puts back the values an entry changed (`Entry::before`): the owning
/// screen makes the edit. Arguments: the entry, the wear to reinstate on
/// those values (tools), the log's cause text.
pub type Restore = Box<dyn Fn(&Entry, [f64; 2], &str) -> Result<(), String>>;

/// `path`: where the log is kept; `None` keeps it in memory (simulator).
pub fn wire(ui: &MainWindow, path: Option<PathBuf>, restore: Restore) {
    HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        WEAR.with(|w| {
            let mut w = w.borrow_mut();
            w.path = path.as_ref().map(|p| p.with_file_name("wear.json"));
            w.tools = w.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        });
        h.path = path;
        h.ui = Some(ui.as_weak());
        h.load();
        h.show();
    });
    let actions = ui.global::<Actions>();
    actions.on_history_select(|i| {
        HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            let id = (i >= 0).then(|| h.entries.iter().rev().nth(i as usize).map(|e| e.id)).flatten();
            h.selected = if h.selected == id { None } else { id };
            h.show();
        });
    });
    let weak = ui.as_weak();
    actions.on_history_restore(move || {
        // Copy what is needed first: the edit below logs a new entry.
        let picked = HISTORY.with(|h| {
            let h = h.borrow();
            let e = h.selected.and_then(|id| h.entries.iter().find(|e| e.id == id))?.clone();
            let wear = wear_before(&h.entries, &e);
            Some((e, wear))
        });
        let Some((e, wear)) = picked else { return };
        let now = now();
        let r = restore(&e, wear, &format!("Restored to before the {} change", e.when(now)));
        if let Some(ui) = weak.upgrade() {
            let live = ui.global::<Live>();
            match r {
                Ok(()) => {
                    live.set_message(format!("{} restored to its values before {}", e.name(), e.when(now)).into());
                    live.set_message_error(false);
                }
                Err(m) => {
                    live.set_message(m.into());
                    live.set_message_error(true);
                }
            }
        }
        HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            h.selected = None;
            h.show();
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, target: Target, old: &[f64], new: &[f64], wear: [f64; 2]) -> Entry {
        Entry { id, time: 0, target, old: old.to_vec(), new: new.to_vec(), cause: String::new(), wear }
    }

    #[test]
    fn change_lists_only_what_moved() {
        let e = entry(1, Target::Wcs(0), &[1.0, 2.0, 3.0, 0.0, 0.0], &[1.0, 2.5, 3.0, 0.0, 15.0], [0.0; 2]);
        assert_eq!(e.change(), "Y 2.000 → 2.500   ROT 0.000 → 15.000");
        assert_eq!(e.name(), "G54");
        let t = entry(2, Target::Tool(7), &[100.0, 6.0], &[99.98, 6.0], [0.0; 2]);
        assert_eq!(t.change(), "LENGTH 100.000 → 99.980");
    }

    #[test]
    fn restore_writes_only_what_the_entry_changed() {
        let e = entry(1, Target::Wcs(0), &[1.0, 2.0, 3.0, 0.0, 0.0], &[1.0, 2.5, 3.0, 0.0, 15.0], [0.0; 2]);
        assert_eq!(e.before(), [(1, 2.0), (4, 0.0)]);
        assert_eq!(e.restores(), "G54 Y 2.000, ROT 0.000");
    }

    #[test]
    fn wear_counts_only_while_the_table_holds_its_value() {
        set_wear(7, 0, -0.02, 99.98);
        assert_eq!(wear(7, [99.98, 6.0]), [-0.02, 0.0]);
        // Re-measured (or edited behind our back): the adjustment is over.
        assert_eq!(wear(7, [100.013, 6.0]), [0.0, 0.0]);
        // A misread that later reads right again does not lose it.
        assert_eq!(wear(7, [99.98, 6.0]), [-0.02, 0.0]);
        set_wear(7, 0, 0.0, 99.98);
        assert_eq!(wear(7, [99.98, 6.0]), [0.0, 0.0]);
        set_wear(8, 1, 0.01, 3.01);
        forget_wear(8);
        assert_eq!(wear(8, [50.0, 3.01]), [0.0, 0.0]);
    }

    #[test]
    fn round_off_is_not_a_change() {
        assert!(!differs(&[1.0, 2.0], &[1.00001, 2.0]));
        assert!(differs(&[1.0, 2.0], &[1.001, 2.0]));
    }

    #[test]
    fn restoring_reinstates_the_wear_from_before() {
        let log = [
            entry(1, Target::Tool(7), &[0.0, 6.0], &[100.0, 6.0], [0.0, 0.0]),
            entry(2, Target::Tool(7), &[100.0, 6.0], &[99.98, 6.0], [-0.02, 0.0]),
            entry(3, Target::Tool(8), &[50.0, 3.0], &[50.1, 3.0], [0.1, 0.0]),
            entry(4, Target::Tool(7), &[99.98, 6.0], &[99.95, 6.0], [-0.05, 0.0]),
        ];
        assert_eq!(wear_before(&log, &log[3]), [-0.02, 0.0]);
        assert_eq!(wear_before(&log, &log[1]), [0.0, 0.0]);
        assert_eq!(wear_before(&log, &log[0]), [0.0, 0.0]);
    }

    #[test]
    fn entries_round_trip_as_json_lines() {
        let e = entry(9, Target::Tool(23), &[1.0, 2.0], &[1.5, 2.0], [0.5, 0.0]);
        let line = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<Entry>(&line).unwrap(), e);
    }
}
