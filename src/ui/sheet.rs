use std::cell::RefCell;
use std::rc::Rc;

use frust::{
    AnyView, CrossAxisAlignment, EdgeInsets, NavigatorController, Padding, PopResult, View, column,
    safe_area,
};
use frust_glyph::{GlyphSheetHandle, glyph_sheet, show_glyph_sheet};

// Re-exported so `ui::sheet::SheetHandle` keeps resolving for every existing
// caller/test — the type itself now lives in `crate::ui`, shared with
// `crate::ui::side_sheet`, since both adapters mint the identical one-shot
// dismissal gate.
pub use super::SheetHandle;

pub const SCRIM_ALPHA: f32 = 0.6;
const PANEL_PAD: f64 = 20.0;

pub fn show_sheet<State: 'static>(
    nav: &NavigatorController<State>,
    build_body: impl Fn(&SheetHandle) -> AnyView<State> + 'static,
) {
    let glyph_handle: Rc<RefCell<Option<GlyphSheetHandle>>> = Rc::new(RefCell::new(None));
    let close_cell = Rc::clone(&glyph_handle);
    let sheet = SheetHandle::new(move || {
        if let Some(handle) = close_cell.borrow().as_ref() {
            handle.close();
        }
    });
    let body_sheet = sheet.clone();
    let close_then_sheet = sheet.clone();
    let handle = show_glyph_sheet(
        nav,
        move || {
            glyph_sheet(sheet_content(build_body(&body_sheet))).on_close({
                let sheet = close_then_sheet.clone();
                move || sheet.run_pending()
            })
        },
        move |_state: &mut State, _result: PopResult| sheet.mark_closed(),
    );
    *glyph_handle.borrow_mut() = Some(handle);
}

fn sheet_content<State: 'static>(body: AnyView<State>) -> impl View<State> {
    safe_area(Padding(
        EdgeInsets {
            left: PANEL_PAD,
            top: 0.0,
            right: PANEL_PAD,
            bottom: PANEL_PAD,
        },
        column().child(body).cross_axis(CrossAxisAlignment::Stretch),
    ))
    .top(false)
    .left(false)
    .right(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scrim_alpha_transcription_stays_legal() {
        assert!((0.0..=1.0).contains(&SCRIM_ALPHA));
    }
}
