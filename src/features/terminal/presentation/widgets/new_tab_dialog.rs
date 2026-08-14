//! The add-tab row's own name dialog — the one prompt in this app where a
//! **blank** field is a valid submit.

use frust::glyph::{GlyphDialogView, glyph_dialog};
use frust::{
    Axis, ButtonStyle, CrossAxisAlignment, FlexView, Get, NavigatorController, PopResult, RwSignal,
    Set, SizedBox, any, button, flexible, inflexible, text_input,
};

use crate::AppState;
use crate::ui::spacing;

/// The dialog's title.
const NEW_TAB_TITLE: &str = "New tab";
/// Its own Cancel/Create labels — a hand-rolled dialog (see
/// [`new_tab_dialog`]), so it spells its own pair rather than reaching into
/// `crate::ui::dialogs`' private ones.
const NEW_TAB_CANCEL_LABEL: &str = "Cancel";
const NEW_TAB_SUBMIT_LABEL: &str = "Create";
/// Vertical gap between the field and its button row, and the horizontal gap
/// between Cancel and Create — the shared scale's own tiers, matching
/// `crate::ui::dialogs::name_input_dialog`'s `FIELD_ACTIONS_GAP`/`ACTION_GAP`
/// (both private there) so the two dialogs sit identically.
const NEW_TAB_FIELD_GAP: f64 = spacing::XL;
const NEW_TAB_ACTION_GAP: f64 = spacing::MD;

/// The tab-naming dialog's page body: a field over a Cancel/Create row.
///
/// A hand-rolled dialog rather than
/// [`name_input_dialog`](crate::ui::dialogs::name_input_dialog): that helper's
/// whole contract is "a blank field cannot submit", which is exactly right for
/// every other name prompt in this app (a rename/create-space needs a real
/// label) and exactly wrong for this one, where blank is this create's own
/// valid default — the server's own naming — rather than a disabled state.
/// Mirrors that helper's own construction otherwise, so `tests/terminal.rs`'s
/// existing dialog-panel idiom (`by_width` against the panel's own content
/// width) still finds the field the identical way.
///
/// `value` is the page's own live buffer: a keystroke's `value.set` is what
/// rebuilds this body, and the trimmed string is what both `Enter` and
/// **Create** pop with. `nav` is the overlay host this page was pushed onto —
/// every one of the three exits pops through it.
pub(crate) fn new_tab_dialog(
    value: RwSignal<String>,
    nav: &NavigatorController<AppState>,
) -> GlyphDialogView<AppState> {
    // Tracked read: a keystroke's `value.set` is what rebuilds this page.
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
    let buttons = FlexView::new(
        Axis::Horizontal,
        vec![
            flexible(1, SizedBox::<AppState>(None, None)),
            inflexible(
                button(NEW_TAB_CANCEL_LABEL, move |_app: &mut AppState| {
                    cancel_nav.pop()
                })
                .style(ButtonStyle::Ghost)
                .small(),
            ),
            inflexible(SizedBox::<AppState>(Some(NEW_TAB_ACTION_GAP), None)),
            inflexible(
                button(NEW_TAB_SUBMIT_LABEL, move |_app: &mut AppState| {
                    submit_nav.pop_with_result(PopResult::of(current.trim().to_string()));
                })
                .style(ButtonStyle::Primary)
                .small(),
            ),
        ],
    );
    glyph_dialog()
        .title(NEW_TAB_TITLE)
        .action(any(FlexView::new(
            Axis::Vertical,
            vec![
                inflexible(field),
                inflexible(SizedBox::<AppState>(None, Some(NEW_TAB_FIELD_GAP))),
                inflexible(buttons),
            ],
        )
        .cross_axis(CrossAxisAlignment::Stretch)))
}
