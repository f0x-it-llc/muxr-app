//! The `/sessions` route's placeholder page: a Glyph [`app_bar`] titled
//! "Sessions" over centered page-name text. Phase 2 replaces this with the
//! real session list.

use frust::glyph::app_bar;
use frust::{
    Align, Alignment, AnyView, Axis, FlexView, NavigatorController, any, flexible, inflexible,
    safe_area, text,
};

use crate::AppState;

/// The Sessions feature's placeholder page. Takes the [`NavigatorController`]
/// (unused today — see `crate::routes::build_routes`'s dual-use shape); a
/// later task pushes through it once Sessions has somewhere to navigate to.
pub fn sessions_page(_controller: NavigatorController<AppState>) -> AnyView<AppState> {
    let bar = any(app_bar::<AppState>("Sessions"));
    // `bar` consumes the top window inset itself (`glyph::app_bar`'s own
    // module docs), so `body`'s safe area disables its own top edge to avoid
    // double-padding it — mirrors `examples/glyph-catalog`'s
    // `variation_frame` composition.
    let body = any(safe_area(Align(Alignment::CENTER, text("Sessions").size(20.0))).top(false));
    any(FlexView::new(
        Axis::Vertical,
        vec![inflexible(bar), flexible(1, body)],
    ))
}
