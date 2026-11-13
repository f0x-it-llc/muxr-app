//! The modal side-sheet adapter: the x-axis sibling of `sheet::show_sheet`, over the same
//! one-shot [`SheetHandle`].

use std::cell::RefCell;
use std::rc::Rc;

use frust::{NavigatorController, PopResult, View};
use frust_glyph::{GlyphSideSheetHandle, glyph_side_sheet, show_glyph_side_sheet};

use super::SheetHandle;

/// `on_dismiss` runs once, after the exit animation completes, on every dismissal, whether a
/// sheet gesture or a programmatic [`SheetHandle::close`] drove it.
pub fn show_side_sheet<State: 'static, V: View<State>, F: View<State>>(
    nav: &NavigatorController<State>,
    title: impl Into<String>,
    build_body: impl Fn(&SheetHandle) -> V + 'static,
    build_footer: impl Fn(&SheetHandle) -> F + 'static,
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
