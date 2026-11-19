//! Updating list properties without redrawing what did not change.
//!
//! Setting a list property to a new `ModelRc` always counts as a change
//! (models compare by pointer), so the repeater rebuilds every row and the
//! window redraws, even when the rows are the same. With the GPU renderer a
//! redraw is the whole frame. `sync` keeps the model the property already
//! has and writes only the rows that differ.

use slint::{Model, ModelRc, VecModel};

/// Show `rows` in a list property: `current` is its getter's value, `set`
/// its setter (called only the first time, to install a `VecModel`).
pub fn sync<T: Clone + PartialEq + 'static>(current: ModelRc<T>, rows: Vec<T>, set: impl FnOnce(ModelRc<T>)) {
    let Some(vm) = current.as_any().downcast_ref::<VecModel<T>>() else {
        set(ModelRc::new(VecModel::from(rows)));
        return;
    };
    if vm.row_count() != rows.len() {
        vm.set_vec(rows);
        return;
    }
    for (i, r) in rows.into_iter().enumerate() {
        if vm.row_data(i).as_ref() != Some(&r) {
            vm.set_row_data(i, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn keeps_the_model_and_writes_only_changes() {
        let slot: RefCell<ModelRc<i32>> = RefCell::new(ModelRc::default());
        let get = || slot.borrow().clone();
        sync(get(), vec![1, 2, 3], |m| *slot.borrow_mut() = m);
        let first = get();
        assert_eq!(first.iter().collect::<Vec<_>>(), [1, 2, 3]);
        sync(get(), vec![1, 5, 3], |_| panic!("model replaced"));
        assert_eq!(get().iter().collect::<Vec<_>>(), [1, 5, 3]);
        sync(get(), vec![4], |_| panic!("model replaced"));
        assert_eq!(get().iter().collect::<Vec<_>>(), [4]);
        assert!(first == get());
    }
}
