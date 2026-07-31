//! Flutter-parity dialog helpers over [`frust::glyph::glyph_dialog`]: a
//! destructive-action confirmation ([`confirm_dialog`]) and a single-field
//! name prompt ([`name_input_dialog`]).
//!
//! Both are one-line call sites — a screen calls one and gets the whole modal
//! lifecycle. The idiom underneath is the catalog's own
//! (`examples/glyph-catalog`'s `pages/overlays.rs`), not a new one:
//! [`show_glyph_dialog`] pushes the dialog as a **transparent navigator page**
//! (the screen below stays visible under the scrim, and the navigator's modal
//! contract is what makes it modal), an action button pops the page carrying
//! its answer (`pop_with_result`), and the pusher's `on_result` callback —
//! the only place with `&mut State` in scope — applies it. A scrim tap,
//! `Escape`, or a back press pops with an *empty* result, which reads as
//! "cancelled" here.
//!
//! # Screen-agnostic
//!
//! Both helpers are generic over `State` (the `ui::backdrop` precedent), so
//! Sessions' kill/rename and Settings' logout all reach for the same two
//! functions with their own callbacks — none of this file knows about
//! `AppState`.
//!
//! # Framework gaps these helpers work around
//!
//! - **No disabled state on `Button`.** `frust::button` offers `.loading(bool)`
//!   (spinner + suppressed press) but no plain disabled variant, so
//!   [`name_input_dialog`]'s submit "disables" by rendering
//!   [`frust::ButtonStyle::Ghost`] and no-op'ing its own `on_press` while the
//!   field is blank (see [`SUBMIT_ENABLED_STYLE`]).
//! - **`glyph_dialog` has no content slot.** Its body is a `String`; arbitrary
//!   content can only be an *action*. [`name_input_dialog`] therefore passes
//!   one action holding the whole field-over-buttons column — laid out
//!   correctly (the action row is right-aligned, and this single action spans
//!   the panel's content width), but plainly a workaround for a missing
//!   `.content(AnyView)` builder.

use frust::glyph::{glyph_dialog, show_glyph_dialog};
use frust::{
    Axis, ButtonStyle, CrossAxisAlignment, FlexView, Get, NavigatorController, PopResult, RwSignal,
    Set, SizedBox, any, button, flexible, inflexible, text_input,
};

/// The shared cancel affordance's label — one spelling for every dialog here.
const CANCEL_LABEL: &str = "Cancel";
/// [`name_input_dialog`]'s submit label — "Save" covers both create and rename.
const SUBMIT_LABEL: &str = "Save";
/// Vertical gap between [`name_input_dialog`]'s field and its button row, in
/// logical px (Glyph's `--space-4`; the dialog's own padding supplies the
/// panel gutter).
const FIELD_ACTIONS_GAP: f64 = 16.0;
/// Horizontal gap between the cancel and submit buttons, in logical px —
/// matches `glyph::dialog`'s own `ACTION_GAP`, since this row stands in for
/// that widget's action row.
const ACTION_GAP: f64 = 10.0;
/// The submit button's style while the field holds a usable name.
const SUBMIT_ENABLED_STYLE: ButtonStyle = ButtonStyle::Primary;
/// …and while it is blank: low-emphasis, paired with a no-op `on_press` (see
/// the [module docs](self)'s framework-gap note).
const SUBMIT_DISABLED_STYLE: ButtonStyle = ButtonStyle::Ghost;

/// Confirm a destructive action: a Glyph modal with `title`/`message`, a ghost
/// **Cancel** and a danger `confirm_label`. `on_confirm` runs (with `&mut
/// State`) only on the confirm path — every dismissal route (cancel button,
/// scrim tap, `Escape`, back press) closes the dialog and runs nothing.
///
/// ```ignore
/// confirm_dialog(
///     &nav,
///     "Kill session?",
///     "Any attached client is disconnected immediately.",
///     "Kill",
///     move |state: &mut AppState| state.sessions.kill(id),
/// );
/// ```
pub fn confirm_dialog<State: 'static>(
    nav: &NavigatorController<State>,
    title: impl Into<String>,
    message: impl Into<String>,
    confirm_label: impl Into<String>,
    on_confirm: impl Fn(&mut State) + 'static,
) {
    let title = title.into();
    let message = message.into();
    let confirm_label = confirm_label.into();
    // Re-invoked on every navigator rebuild to diff the page's content, so
    // every captured value is cloned per call rather than moved.
    let page_nav = nav.clone();
    show_glyph_dialog(
        nav,
        move || {
            let cancel_nav = page_nav.clone();
            let confirm_nav = page_nav.clone();
            glyph_dialog()
                .title(title.clone())
                .body(message.clone())
                .action(any(button(CANCEL_LABEL, move |_state: &mut State| {
                    cancel_nav.pop()
                })
                .style(ButtonStyle::Ghost)
                .small()))
                .action(any(button(
                    confirm_label.clone(),
                    move |_state: &mut State| confirm_nav.pop_with_result(PopResult::of(true)),
                )
                .style(ButtonStyle::Danger)
                .small()))
        },
        move |state: &mut State, result: PopResult| {
            // Only the confirm action pops with a payload; every dismissal
            // route pops empty.
            if result.take::<bool>().unwrap_or(false) {
                on_confirm(state);
            }
        },
    );
}

/// Prompt for a single line of text (session create/rename): a Glyph modal
/// with `title`, a [`text_input`] seeded with `initial`, a ghost **Cancel**
/// and a submit button.
///
/// `on_submit` receives the **trimmed**, non-empty text. A blank (or
/// whitespace-only) field cannot submit — neither the button nor the field's
/// own Enter key does anything — and every dismissal route (cancel, scrim,
/// `Escape`, back) closes the dialog without calling back.
///
/// The in-flight text lives in an [`RwSignal`] created here rather than in app
/// state: the field is a controlled component, and this signal is the only
/// thing that has to survive the dialog page's rebuilds.
pub fn name_input_dialog<State: 'static>(
    nav: &NavigatorController<State>,
    title: impl Into<String>,
    initial: impl Into<String>,
    on_submit: impl Fn(&mut State, String) + 'static,
) {
    let title = title.into();
    let value = RwSignal::new(initial.into());
    let page_nav = nav.clone();
    show_glyph_dialog(
        nav,
        move || {
            // Tracked read: a keystroke's `value.set` is what rebuilds this
            // page with the new text (and re-evaluates `can_submit`).
            let current = value.get();
            let can_submit = submittable(&current);
            let enter_nav = page_nav.clone();
            let submit_nav = page_nav.clone();
            let cancel_nav = page_nav.clone();
            let field = text_input(current.clone(), move |_state: &mut State, next: String| {
                value.set(next)
            })
            .on_submit(move |_state: &mut State, typed: String| submit(&enter_nav, &typed));
            let buttons = FlexView::new(
                Axis::Horizontal,
                vec![
                    // Flex v1 has no space-between (`MainAxisAlignment::Start`
                    // only), so a flexible spacer is what pins the pair to the
                    // trailing edge — `features::connect`'s `remember_row`
                    // uses the same trick.
                    flexible(1, SizedBox::<State>(None, None)),
                    inflexible(
                        button(CANCEL_LABEL, move |_state: &mut State| cancel_nav.pop())
                            .style(ButtonStyle::Ghost)
                            .small(),
                    ),
                    inflexible(SizedBox::<State>(Some(ACTION_GAP), None)),
                    inflexible(
                        button(SUBMIT_LABEL, move |_state: &mut State| {
                            // "Disabled": the style below is the visual half,
                            // this guard the behavioral one (see the module
                            // docs' framework-gap note).
                            if can_submit {
                                submit(&submit_nav, &current);
                            }
                        })
                        .style(if can_submit {
                            SUBMIT_ENABLED_STYLE
                        } else {
                            SUBMIT_DISABLED_STYLE
                        })
                        .small(),
                    ),
                ],
            );
            glyph_dialog()
                .title(title.clone())
                .action(any(FlexView::new(
                    Axis::Vertical,
                    vec![
                        inflexible(field),
                        inflexible(SizedBox::<State>(None, Some(FIELD_ACTIONS_GAP))),
                        inflexible(buttons),
                    ],
                )
                // Stretch so the field spans the panel's content width — see
                // the module docs' "no content slot" note for why this whole
                // column is a single dialog *action*.
                .cross_axis(CrossAxisAlignment::Stretch)))
        },
        move |state: &mut State, result: PopResult| {
            if let Some(name) = result.take::<String>() {
                on_submit(state, name);
            }
        },
    );
}

/// Whether `text` is a usable name (non-blank once trimmed).
fn submittable(text: &str) -> bool {
    !text.trim().is_empty()
}

/// Pop the dialog page carrying the trimmed name, or do nothing when there is
/// nothing to submit — the one gate both the Enter key and the submit button
/// route through.
fn submit<State: 'static>(nav: &NavigatorController<State>, typed: &str) {
    let name = typed.trim();
    if name.is_empty() {
        return;
    }
    nav.pop_with_result(PopResult::of(name.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_non_blank_name_is_submittable() {
        assert!(!submittable(""));
        assert!(!submittable("   "));
        assert!(!submittable("\t\n"));
        assert!(submittable("dev"));
        assert!(submittable("  dev  "));
    }

    #[test]
    fn the_disabled_submit_style_is_lower_emphasis_than_the_enabled_one() {
        // A tripwire on the pair, not on either constant: the "disabled" look
        // only reads as disabled while it differs from the enabled one.
        assert_ne!(SUBMIT_ENABLED_STYLE, SUBMIT_DISABLED_STYLE);
    }
}
