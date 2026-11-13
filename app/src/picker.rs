//! A touch list to pick one item from (OPEN JOB, ADD STEP, conversational
//! choices). One picker is open at a time; its owner gets the index.

use crate::{Actions, Live, MainWindow};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;

type Handler = Box<dyn Fn(usize)>;

thread_local! {
    static OWNER: RefCell<String> = const { RefCell::new(String::new()) };
    static HANDLERS: RefCell<Vec<(String, Handler)>> = const { RefCell::new(Vec::new()) };
}

pub fn open(ui: &MainWindow, owner: &str, title: &str, items: Vec<String>, current: Option<usize>) {
    OWNER.with(|o| *o.borrow_mut() = owner.to_string());
    let live = ui.global::<Live>();
    live.set_pick_title(title.into());
    let items: Vec<SharedString> = items.into_iter().map(SharedString::from).collect();
    live.set_pick_items(ModelRc::new(VecModel::from(items)));
    live.set_pick_current(current.map_or(-1, |i| i as i32));
    live.set_pick_open(true);
}

pub fn on_pick(owner: &str, f: impl Fn(usize) + 'static) {
    HANDLERS.with(|h| h.borrow_mut().push((owner.to_string(), Box::new(f))));
}

pub fn wire(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.global::<Actions>().on_pick(move |i| {
        if let Some(ui) = weak.upgrade() {
            ui.global::<Live>().set_pick_open(false);
        }
        let owner = OWNER.with(|o| std::mem::take(&mut *o.borrow_mut()));
        if i < 0 {
            return;
        }
        HANDLERS.with(|h| {
            if let Some((_, f)) = h.borrow().iter().find(|(o, _)| *o == owner) {
                f(i as usize);
            }
        });
    });
}
