//! The `/terminal` route's placeholder page: a Glyph [`app_bar`] titled
//! "Terminal" over centered page-name text. Phase 2 replaces this with the
//! real terminal surface.

use frust::glyph::app_bar;
use frust::{
    Align, Alignment, AnyView, Axis, FlexView, any, flexible, inflexible, safe_area, text,
};

use crate::AppState;

/// The Terminal feature's placeholder page.
pub fn terminal_page() -> AnyView<AppState> {
    let bar = any(app_bar::<AppState>("Terminal"));
    // `bar` consumes the top window inset itself (`glyph::app_bar`'s own
    // module docs), so `body`'s safe area disables its own top edge to avoid
    // double-padding it — mirrors `examples/glyph-catalog`'s
    // `variation_frame` composition.
    let body = any(safe_area(Align(Alignment::CENTER, text("Terminal").size(20.0))).top(false));
    any(FlexView::new(
        Axis::Vertical,
        vec![inflexible(bar), flexible(1, body)],
    ))
}
