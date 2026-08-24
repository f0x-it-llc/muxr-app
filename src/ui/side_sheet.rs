//! The modal side-sheet adapter over `frust_glyph`'s own side sheet — the
//! x-axis sibling of [`crate::ui::sheet::show_sheet`], minting the identical
//! one-shot [`SheetHandle`] dismissal gate (`close`/`close_after`/
//! `close_then` fire at most once) over
//! [`frust_glyph::show_glyph_side_sheet`]'s own staged exit.
//!
//! Unlike the bottom sheet, a side sheet also owes its caller a way to react
//! to *any* dismissal (scrim tap, the close affordance, `Escape`, back, or a
//! programmatic close) rather than only the ones the caller itself drove —
//! `on_dismiss` is that seam, a bare `Fn()` like
//! [`frust_glyph::GlyphSideSheetView::on_close`] itself, so it stays state-free
//! like every other callback this module and [`crate::ui::sheet`] cross the
//! overlay-host boundary with.

use std::cell::RefCell;
use std::rc::Rc;

use frust::{AnyView, NavigatorController, PopResult};
use frust_glyph::{GlyphSideSheetHandle, glyph_side_sheet, show_glyph_side_sheet};

use super::SheetHandle;

/// Push `title`'s side sheet over `build_body` (the scrolling containment
/// tree) and `build_footer` (the sticky footer pinned to the panel's bottom
/// edge), minting a [`SheetHandle`] both builders are handed so any row/action
/// inside either can close the sheet through the same one-shot gate
/// [`crate::ui::sheet::show_sheet`]'s own callers use. `on_dismiss` runs once,
/// after the exit animation completes, on *every* dismissal — whichever of
/// the sheet's own gestures or a programmatic [`SheetHandle::close`] drove it.
pub fn show_side_sheet<State: 'static>(
    nav: &NavigatorController<State>,
    title: impl Into<String>,
    build_body: impl Fn(&SheetHandle) -> AnyView<State> + 'static,
    build_footer: impl Fn(&SheetHandle) -> AnyView<State> + 'static,
    on_dismiss: impl Fn() + 'static,
) {
    let title = title.into();
    let on_dismiss = Rc::new(on_dismiss);
    let glyph_handle: Rc<RefCell<Option<GlyphSideSheetHandle>>> = Rc::new(RefCell::new(None));
    let close_cell = Rc::clone(&glyph_handle);
    let sheet = SheetHandle::new(move || {
        if let Some(handle) = close_cell.borrow().as_ref() {
            handle.close();
        }
    });
    let body_sheet = sheet.clone();
    let footer_sheet = sheet.clone();
    let close_then_sheet = sheet.clone();
    let handle = show_glyph_side_sheet(
        nav,
        move || {
            let sheet = close_then_sheet.clone();
            let on_dismiss = Rc::clone(&on_dismiss);
            glyph_side_sheet(title.clone(), build_body(&body_sheet))
                .footer(build_footer(&footer_sheet))
                .on_close(move || {
                    sheet.run_pending();
                    on_dismiss();
                })
        },
        move |_state: &mut State, _result: PopResult| sheet.mark_closed(),
    );
    *glyph_handle.borrow_mut() = Some(handle);
}
