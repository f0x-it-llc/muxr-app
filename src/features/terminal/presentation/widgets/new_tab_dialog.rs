//! The add-tab row's own name dialog — the one prompt in this app where a
//! **blank** field is a valid submit.

use frust::{
    ButtonStyle, CrossAxisAlignment, Get, NavigatorController, PopResult, RwSignal, Set, SizedBox,
    button, column, row, text_input,
};
use frust_glyph::{GlyphDialogView, glyph_dialog};

use crate::AppState;
use crate::ui::spacing;

/// The dialog's title.
const NEW_TAB_TITLE: &str = "New tab";
/// Its own Cancel/Create labels — a hand-rolled dialog (see
/// [`new_tab_dialog`]), so it spells its own pair rather than reaching into
/// `crate::ui::dialogs`' private ones.
const NEW_TAB_CANCEL_LABEL: &str = "Cancel";
const NEW_TAB_SUBMIT_LABEL: &str = "Create";
/// Field-to-buttons and Cancel-to-Create gaps — matching
/// `crate::ui::dialogs::name_input_dialog`'s own (both private there).
const NEW_TAB_FIELD_GAP: f64 = spacing::XL;
const NEW_TAB_ACTION_GAP: f64 = spacing::MD;

/// The tab-naming dialog's page body: a field over a Cancel/Create row.
///
/// Hand-rolled rather than
/// [`name_input_dialog`](crate::ui::dialogs::name_input_dialog): that
/// helper's whole contract is "a blank field cannot submit", exactly wrong
/// here — blank is this create's own valid default, the server's own
/// naming. Mirrors that helper's construction otherwise, so
/// `tests/terminal.rs`'s dialog-panel idiom still finds the field the same
/// way.
///
/// `value` is the page's own live buffer: a keystroke's `value.set` rebuilds
/// this body, and the trimmed string is what both `Enter` and **Create** pop
/// with.
pub(crate) fn new_tab_dialog(
    value: RwSignal<String>,
    nav: &NavigatorController<AppState>,
) -> GlyphDialogView<AppState> {
    // Tracked: a keystroke's `value.set` rebuilds this page.
    let current = value.get();
    let enter_nav = nav.clone();
    let submit_nav = nav.clone();
    let cancel_nav = nav.clone();
    let field = text_input(current.clone(), move |_app: &mut AppState, next: String| {
        value.set(next)
    })
    .on_submit(move |_app: &mut AppState, typed: String| {
        enter_nav.pop_with_result(PopResult::of(typed.trim().to_string()));
    });
    let buttons = row()
        .flex(1, SizedBox::<AppState>(None, None))
        .child(
            button(NEW_TAB_CANCEL_LABEL, move |_app: &mut AppState| {
                cancel_nav.pop()
            })
            .style(ButtonStyle::Ghost)
            .small(),
        )
        .child(SizedBox::<AppState>(Some(NEW_TAB_ACTION_GAP), None))
        .child(
            button(NEW_TAB_SUBMIT_LABEL, move |_app: &mut AppState| {
                submit_nav.pop_with_result(PopResult::of(current.trim().to_string()));
            })
            .style(ButtonStyle::Primary)
            .small(),
        );
    glyph_dialog().title(NEW_TAB_TITLE).action(
        column()
            .child(field)
            .child(SizedBox::<AppState>(None, Some(NEW_TAB_FIELD_GAP)))
            .child(buttons)
            .cross_axis(CrossAxisAlignment::Stretch),
    )
}
