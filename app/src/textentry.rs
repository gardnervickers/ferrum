//! The on-screen text keyboard, for short text such as tool comments.
//! Numbers use the keypad (manual.rs); this is for words.

use crate::{Actions, Live, MainWindow};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;

/// Longest text the keyboard takes (the table's longest comment is ~140).
pub const MAX_CHARS: usize = 160;

/// A screen's text values edited on the keyboard. Each answers only for the
/// names it owns (`None` otherwise).
pub trait TextTarget {
    fn label(&self, what: &str) -> Option<String>;
    fn current(&self, what: &str) -> Option<String>;
    /// Check and store; `Err` keeps the keyboard open with the message.
    fn apply(&self, what: &str, v: &str) -> Option<Result<(), String>>;
}

#[derive(Default)]
struct Entry {
    what: String,
    label: String,
    buf: String,
    caps: bool,
    error: Option<String>,
}

thread_local! {
    static TARGETS: RefCell<Vec<Rc<dyn TextTarget>>> = const { RefCell::new(Vec::new()) };
}

pub fn add_target(t: Rc<dyn TextTarget>) {
    TARGETS.with(|k| k.borrow_mut().push(t));
}

/// One key: a character (already in the case shown on the key), or
/// "⌫", "CLR", "⇧" (caps on/off), "SPACE".
fn key(e: &mut Entry, k: &str) {
    e.error = None;
    match k {
        "⌫" => {
            e.buf.pop();
        }
        "CLR" => e.buf.clear(),
        "⇧" => e.caps = !e.caps,
        "SPACE" => push(e, " "),
        _ => push(e, k),
    }
}

fn push(e: &mut Entry, s: &str) {
    // One character at a time; control characters (Tab, arrows from a
    // hardware keyboard) and anything past the limit are ignored.
    for c in s.chars().filter(|c| !c.is_control()) {
        if e.buf.chars().count() >= MAX_CHARS {
            e.error = Some(format!("At most {MAX_CHARS} characters"));
            return;
        }
        e.buf.push(c);
    }
}

fn show(ui: &MainWindow, e: Option<&Entry>) {
    let live = ui.global::<Live>();
    live.set_text_open(e.is_some());
    if let Some(e) = e {
        live.set_text_label(e.label.clone().into());
        live.set_text_value(e.buf.clone().into());
        live.set_text_caps(e.caps);
        live.set_text_error(e.error.clone().unwrap_or_default().into());
    }
}

pub fn wire(ui: &MainWindow) {
    let entry: Rc<RefCell<Option<Entry>>> = Rc::new(RefCell::new(None));
    let actions = ui.global::<Actions>();
    let (en, weak) = (entry.clone(), ui.as_weak());
    actions.on_text_edit(move |what| {
        let found = TARGETS.with(|t| {
            t.borrow().iter().find_map(|t| Some((t.label(&what)?, t.current(&what).unwrap_or_default())))
        });
        let Some((label, buf)) = found else { return };
        *en.borrow_mut() = Some(Entry { what: what.into(), label, buf, caps: true, error: None });
        if let Some(ui) = weak.upgrade() {
            show(&ui, en.borrow().as_ref());
        }
    });
    let (en, weak) = (entry.clone(), ui.as_weak());
    actions.on_text_key(move |k| {
        if let Some(e) = en.borrow_mut().as_mut() {
            key(e, &k);
        }
        if let Some(ui) = weak.upgrade() {
            show(&ui, en.borrow().as_ref());
        }
    });
    let (en, weak) = (entry, ui.as_weak());
    actions.on_text_done(move |ok| {
        let Some(ui) = weak.upgrade() else { return };
        if ok {
            let r = en.borrow().as_ref().and_then(|e| {
                let v = e.buf.trim().to_string();
                TARGETS.with(|t| t.borrow().iter().find_map(|t| t.apply(&e.what, &v)))
            });
            if let Some(Err(err)) = r {
                if let Some(e) = en.borrow_mut().as_mut() {
                    e.error = Some(err);
                }
                show(&ui, en.borrow().as_ref());
                return;
            }
        }
        *en.borrow_mut() = None;
        show(&ui, None);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_edit_the_text() {
        let mut e = Entry { buf: "Jobber".into(), ..Default::default() };
        for k in ["SPACE", "6", ".", "0", "⌫", "5"] {
            key(&mut e, k);
        }
        assert_eq!(e.buf, "Jobber 6.5");
        key(&mut e, "CLR");
        assert_eq!(e.buf, "");
        key(&mut e, "⇧");
        assert!(e.caps);
    }

    #[test]
    fn length_is_limited_and_control_characters_dropped() {
        let mut e = Entry::default();
        key(&mut e, "\t");
        assert_eq!(e.buf, "");
        for _ in 0..MAX_CHARS + 5 {
            key(&mut e, "x");
        }
        assert_eq!(e.buf.chars().count(), MAX_CHARS);
        assert!(e.error.is_some());
    }
}
