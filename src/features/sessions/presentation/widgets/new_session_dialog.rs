//! The "New session" dialog: a name field and, only when the server fronts
//! more than one multiplexer, a `zellij | herdr` segmented choice
//! (`docs/tablet.html` frame 02). With a single backend the dialog is the
//! plain name prompt `ui::dialogs::name_input_dialog` draws.
//!
//! Composed over `glyph_dialog`/`show_glyph_dialog` directly because that
//! prompt has no slot for a second control. Every dismissal route (Cancel,
//! scrim, `Escape`, back) closes without calling back; Enter and Create both
//! go through [`submit`], which refuses a blank name like the repository does.

use frust::{
    Axis, ButtonStyle, CrossAxisAlignment, FlexView, Get, GetUntracked, NavigatorController,
    PopResult, RwSignal, Set, SizedBox, any, button, inflexible, text_input,
};
use frust_glyph::{glyph_dialog, segmented_control, show_glyph_dialog};

use crate::AppState;
use crate::connection::ServerBackend;
use crate::features::sessions::domain::entities::SessionBackend;
use crate::ui::spacing;

const TITLE: &str = "New session";
const CANCEL_LABEL: &str = "Cancel";
const SUBMIT_LABEL: &str = "Create";
/// Vertical gap between the name field and the backend choice.
const FIELD_GAP: f64 = spacing::XL;

/// The offered backends as the session domain names them, zellij first (the
/// preferred default) and each listed once.
pub(in crate::features::sessions::presentation) fn offered(
    available: &[ServerBackend],
) -> Vec<SessionBackend> {
    let mut out = Vec::new();
    for wanted in [ServerBackend::Zellij, ServerBackend::Herdr] {
        if available.contains(&wanted) {
            out.push(match wanted {
                ServerBackend::Zellij => SessionBackend::Zellij,
                ServerBackend::Herdr => SessionBackend::Herdr,
            });
        }
    }
    if out.is_empty() {
        out.push(SessionBackend::Zellij);
    }
    out
}

/// Open the dialog over `overlays`. `on_submit` receives the trimmed,
/// non-empty name and the chosen backend.
pub(in crate::features::sessions::presentation) fn open_new_session_dialog(
    overlays: &NavigatorController<AppState>,
    available: &[ServerBackend],
    on_submit: impl Fn(&mut AppState, String, SessionBackend) + 'static,
) {
    let backends = offered(available);
    let name = RwSignal::new(String::new());
    let choice = RwSignal::new(0usize);
    let page_nav = overlays.clone();
    let page_backends = backends.clone();
    show_glyph_dialog(
        overlays,
        move || {
            let current = name.get();
            let selected = choice.get();
            let can_submit = !current.trim().is_empty();
            let enter_nav = page_nav.clone();
            let submit_nav = page_nav.clone();
            let cancel_nav = page_nav.clone();
            let enter_backends = page_backends.clone();
            let submit_backends = page_backends.clone();
            let submit_current = current.clone();
            let field = text_input(current, move |_state: &mut AppState, next: String| {
                name.set(next)
            })
            .on_submit(move |_state: &mut AppState, typed: String| {
                submit(
                    &enter_nav,
                    &typed,
                    enter_backends[choice.get_untracked().min(enter_backends.len() - 1)],
                )
            });
            let mut rows = vec![inflexible(field)];
            if page_backends.len() > 1 {
                let labels = page_backends
                    .iter()
                    .map(|b| b.label().to_string())
                    .collect();
                rows.push(inflexible(SizedBox::<AppState>(None, Some(FIELD_GAP))));
                rows.push(inflexible(segmented_control(
                    labels,
                    selected,
                    move |_state: &mut AppState, index: usize| choice.set(index),
                )));
            }
            glyph_dialog()
                .title(TITLE)
                .content(any(
                    FlexView::new(Axis::Vertical, rows).cross_axis(CrossAxisAlignment::Stretch)
                ))
                .action(any(button(CANCEL_LABEL, move |_state: &mut AppState| {
                    cancel_nav.pop()
                })
                .style(ButtonStyle::Ghost)
                .small()))
                .action(any(button(SUBMIT_LABEL, move |_state: &mut AppState| {
                    if can_submit {
                        submit(
                            &submit_nav,
                            &submit_current,
                            submit_backends[choice.get_untracked().min(submit_backends.len() - 1)],
                        );
                    }
                })
                .style(if can_submit {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Ghost
                })
                .small()))
        },
        move |state: &mut AppState, result: PopResult| {
            if let Some((typed, backend)) = result.take::<(String, SessionBackend)>() {
                on_submit(state, typed, backend);
            }
        },
    );
}

/// Pop the dialog carrying the trimmed name and backend, or do nothing with a
/// blank name — the one gate Enter and Create share.
fn submit(nav: &NavigatorController<AppState>, typed: &str, backend: SessionBackend) {
    let name = typed.trim();
    if name.is_empty() {
        return;
    }
    nav.pop_with_result(PopResult::of((name.to_string(), backend)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zellij_leads_and_a_lone_or_empty_offer_still_yields_one_backend() {
        assert_eq!(
            offered(&[ServerBackend::Herdr, ServerBackend::Zellij]),
            vec![SessionBackend::Zellij, SessionBackend::Herdr]
        );
        assert_eq!(
            offered(&[ServerBackend::Herdr]),
            vec![SessionBackend::Herdr]
        );
        assert_eq!(offered(&[]), vec![SessionBackend::Zellij]);
    }
}
