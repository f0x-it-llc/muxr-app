//! The stage's action bar: lock, add, close, over frust-glyph's `switcher_bar`/`switcher_chip`.
//! It pads and consumes the bottom safe-area inset itself, so it is handed to `action_bar` bare.

use std::rc::Rc;

use frust::View;
use frust::icons;
use frust_glyph::{switcher_bar, switcher_chip};

pub const LOCK_LABEL: &str = "Lock keyring";
pub const ADD_LABEL: &str = "add server";
pub const CLOSE_LABEL: &str = "Close";

pub fn servers_bar<S: 'static>(
    on_lock: Rc<dyn Fn()>,
    on_add: Rc<dyn Fn()>,
    on_close: Rc<dyn Fn()>,
) -> impl View<S> {
    switcher_bar((
        switcher_chip(icons::LOCK, move |_state: &mut S| on_lock()).semantic_label(LOCK_LABEL),
        switcher_chip(icons::ADD, move |_state: &mut S| on_add()).label(ADD_LABEL),
        switcher_chip(icons::CLOSE, move |_state: &mut S| on_close()).semantic_label(CLOSE_LABEL),
    ))
}
