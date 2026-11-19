//! MDI tab: command line, a help line, a library of common codes by
//! category (mdi.toml, editable) and a G-code keypad. Every entry is plain
//! G-code sent through LinuxCNC MDI.

use crate::config::Config;
use crate::{Actions, Live, MainWindow, MdiCard};
use machine::{Command, Handle};
use serde::Deserialize;
use slint::{ComponentHandle, SharedString};
use std::cell::RefCell;
use std::rc::Rc;

const BUILTIN: &str = include_str!("../../mdi.toml");
const HISTORY: usize = 50;
pub const RECENT: &str = "RECENT";

#[derive(Deserialize, Clone, Debug)]
pub struct Entry {
    pub category: String,
    pub label: String,
    pub description: String,
    /// A shorter description for the card (one line of about 40 characters);
    /// the full one is the help line. Optional.
    #[serde(default)]
    pub brief: String,
    pub fill: String,
    #[serde(default)]
    pub moves: bool,
}

impl Entry {
    /// What the card shows under the label.
    pub fn card_text(&self) -> &str {
        if self.brief.is_empty() { &self.description } else { &self.brief }
    }
}

#[derive(Deserialize, Debug)]
pub struct Library {
    pub categories: Vec<String>,
    #[serde(rename = "entry", default)]
    pub entries: Vec<Entry>,
}

impl Library {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// `[display]mdi_library` (or `[FERRUM]MDI_LIBRARY`), else `ferrum/mdi.toml` beside the INI, else
    /// the copy built into the binary.
    fn load(cfg: &Config, notes: &mut Vec<String>) -> Self {
        let path = crate::machine_cfg::get()
            .mdi_library(cfg)
            .map(|p| cfg.dir().join(p))
            .unwrap_or_else(|| cfg.dir().join("ferrum/mdi.toml"));
        match std::fs::read_to_string(&path) {
            Ok(t) => Self::parse(&t).unwrap_or_else(|e| {
                notes.push(format!("{}: {e}", path.display()));
                Self::builtin()
            }),
            Err(_) => Self::builtin(),
        }
    }

    pub fn builtin() -> Self {
        Self::parse(BUILTIN).expect("built-in mdi.toml")
    }

    /// The entry that best explains a command: its fill words match the
    /// command's leading words. Parameter words match by letter, so
    /// "G10 L20 P2 X5" is explained by "G10 L20 P1 X". A command shorter than
    /// a fill ("G4") matches as a partial.
    pub fn explain(&self, text: &str) -> Option<&Entry> {
        let t = normalize(text);
        let tt: Vec<&str> = t.split(' ').filter(|w| !w.is_empty()).collect();
        let mut best: Option<(usize, &Entry)> = None;
        for e in &self.entries {
            let f = normalize(&e.fill);
            let ft: Vec<&str> = f.split(' ').filter(|w| !w.is_empty()).collect();
            let n = ft.len().min(tt.len());
            if n == 0 || !(0..n).all(|i| word_match(ft[i], tt[i])) {
                continue;
            }
            let score = if tt.len() >= ft.len() { ft.len() * 2 + 1 } else { n * 2 };
            if best.is_none_or(|(b, _)| score > b) {
                best = Some((score, e));
            }
        }
        best.map(|(_, e)| e)
    }
}

/// G, M and L words must match exactly; value words (X, P, S ...) by letter.
fn word_match(fill: &str, typed: &str) -> bool {
    if fill == typed {
        return true;
    }
    let Some(letter) = fill.chars().next() else { return false };
    "XYZABCFSTHPQRIJKD".contains(letter)
        && typed.starts_with(letter)
        && (fill.len() == 1 || fill[1..].parse::<f64>().is_ok())
        && (typed.len() == 1 || typed[1..].parse::<f64>().is_ok())
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_uppercase()
}

/// The help line for what is typed.
pub fn help(lib: &Library, text: &str) -> String {
    let t = text.trim_end();
    if t.is_empty() {
        return "Type a command or pick one below. Tap SEND to run it through MDI.".into();
    }
    let mut out = lib.explain(t).map(|e| e.description.clone()).unwrap_or_else(|| "Not in the library; sent as typed".into());
    if t.ends_with(|c: char| c.is_ascii_alphabetic()) {
        out.push_str(" · type the value on the keypad");
    }
    out
}

/// G-code keypad. A letter starts a new word, so "X5Y0" becomes "X5 Y0".
pub fn key(text: &str, k: &str) -> String {
    let mut t = text.to_string();
    match k {
        "⌫" => {
            t.pop();
        }
        "CLR" => t.clear(),
        "SPACE" => {
            if !t.is_empty() && !t.ends_with(' ') {
                t.push(' ');
            }
        }
        "−" | "-" => t.push('-'),
        _ if k.chars().all(|c| c.is_ascii_alphabetic()) => {
            if !t.is_empty() && !t.ends_with(' ') {
                t.push(' ');
            }
            t.push_str(k);
        }
        _ => t.push_str(k),
    }
    t
}

struct Mdi {
    lib: Library,
    category: String,
    history: Vec<String>,
}

impl Mdi {
    fn cards(&self) -> Vec<MdiCard> {
        if self.category == RECENT {
            return self
                .history
                .iter()
                .map(|h| {
                    let desc = self.lib.explain(h).map(|e| e.card_text().to_string()).unwrap_or_default();
                    let moves = self.lib.explain(h).is_some_and(|e| e.moves);
                    MdiCard { label: h.into(), desc: desc.into(), fill: h.into(), moves }
                })
                .collect();
        }
        self.lib
            .entries
            .iter()
            .filter(|e| e.category == self.category)
            .map(|e| MdiCard {
                label: e.label.clone().into(),
                desc: e.card_text().into(),
                fill: e.fill.clone().into(),
                moves: e.moves,
            })
            .collect()
    }

    fn show(&self, live: &Live) {
        live.set_mdi_category(self.category.clone().into());
        crate::models::sync(live.get_mdi_cards(), self.cards(), |m| live.set_mdi_cards(m));
        live.set_mdi_help(help(&self.lib, live.get_mdi_text().as_str()).into());
    }
}

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, notes: &mut Vec<String>) {
    let lib = Library::load(cfg, notes);
    let mut cats: Vec<SharedString> = vec![RECENT.into()];
    cats.extend(lib.categories.iter().map(SharedString::from));
    let first = lib.categories.first().cloned().unwrap_or_else(|| RECENT.into());
    let state = Rc::new(RefCell::new(Mdi { lib, category: first, history: Vec::new() }));
    let live = ui.global::<Live>();
    crate::models::sync(live.get_mdi_categories(), cats, |m| live.set_mdi_categories(m));
    state.borrow().show(&live);

    let actions = ui.global::<Actions>();
    let redraw = {
        let (st, weak) = (state.clone(), ui.as_weak());
        move || {
            if let Some(ui) = weak.upgrade() {
                st.borrow().show(&ui.global::<Live>());
            }
        }
    };
    let (st, r) = (state.clone(), redraw.clone());
    actions.on_mdi_category_set(move |c| {
        st.borrow_mut().category = c.into();
        r();
    });
    let (weak, r) = (ui.as_weak(), redraw.clone());
    actions.on_mdi_pick(move |fill| {
        if let Some(ui) = weak.upgrade() {
            ui.global::<Live>().set_mdi_text(fill);
        }
        r();
    });
    let (weak, r) = (ui.as_weak(), redraw.clone());
    actions.on_mdi_key(move |k| {
        if let Some(ui) = weak.upgrade() {
            let live = ui.global::<Live>();
            live.set_mdi_text(key(live.get_mdi_text().as_str(), &k).into());
        }
        r();
    });
    let r = redraw.clone();
    actions.on_mdi_edited(move || r());
    let (mc, st, weak) = (machine.clone(), state.clone(), ui.as_weak());
    actions.on_mdi_send(move |text| {
        let line = normalize(&text);
        if line.is_empty() {
            return;
        }
        {
            let mut s = st.borrow_mut();
            s.history.retain(|h| *h != line);
            s.history.insert(0, line.clone());
            s.history.truncate(HISTORY);
        }
        mc.send(Command::Mdi(line));
        if let Some(ui) = weak.upgrade() {
            let live = ui.global::<Live>();
            live.set_mdi_text("".into());
            st.borrow().show(&live);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_library_parses() {
        let lib = Library::builtin();
        assert!(lib.entries.len() > 20);
        for e in &lib.entries {
            assert!(lib.categories.contains(&e.category), "{} has unknown category {}", e.label, e.category);
        }
    }

    #[test]
    fn help_matches_longest_prefix() {
        let lib = Library::builtin();
        assert!(help(&lib, "g10 l20 p1 x5 y0").starts_with("Set G54 so the current spot"));
        assert!(help(&lib, "G10 L2 P1 X0").starts_with("Set the G54 origin"));
        assert!(help(&lib, "G0 X").ends_with("type the value on the keypad"));
        assert_eq!(help(&lib, "G53 G0 Z0"), "Retract Z to the top of travel");
        assert!(help(&lib, "G38.2 Z-10").starts_with("Not in the library"));
        assert!(help(&lib, "G10 L20 P2 X0").starts_with("Set G54 so the current spot"));
        assert_eq!(help(&lib, "G4"), "Dwell P seconds");
        assert_eq!(help(&lib, "G40"), "Cutter compensation off (probing needs it off)");
        assert_eq!(help(&lib, "S8000 M3"), "Spindle CW at S rpm (max 4250)");
    }

    #[test]
    fn keypad_separates_words() {
        let mut t = String::new();
        for k in ["G", "0", "X", "5", "Y", "−", "2", ".", "5"] {
            t = key(&t, k);
        }
        assert_eq!(t, "G0 X5 Y-2.5");
        assert_eq!(key("G0 ", "SPACE"), "G0 ");
        assert_eq!(key("G0", "⌫"), "G");
    }
}
