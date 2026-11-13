//! The stage's action bar: lock, add, close.
//!
//! The strip, the 48 px icon chips and the >= 128 px labelled chip are
//! frust-glyph's `switcher_bar` / `switcher_chip`; muxr supplies only the
//! three vector icons, the labels and the callbacks. The bar pads and
//! consumes the bottom safe-area inset itself, so it is handed to the
//! switcher's `action_bar` bare.

use std::rc::Rc;

use frust::View;
use frust::icons;
use frust_glyph::{switcher_bar, switcher_chip};

/// The lock chip's accessible name.
pub const LOCK_LABEL: &str = "Lock keyring";
/// The add chip's visible label.
pub const ADD_LABEL: &str = "add server";
/// The close chip's accessible name.
pub const CLOSE_LABEL: &str = "Close";

/// Lock & disconnect, add a server, close the stage.
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
