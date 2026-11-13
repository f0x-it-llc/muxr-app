use frust::{ButtonStyle, Get, NavigatorController, PopResult, RwSignal, Set, button, text_input};
use frust_glyph::{glyph_dialog, show_glyph_dialog};

const CANCEL_LABEL: &str = "Cancel";
const SUBMIT_LABEL: &str = "Save";
const SUBMIT_ENABLED_STYLE: ButtonStyle = ButtonStyle::Primary;
const SUBMIT_DISABLED_STYLE: ButtonStyle = ButtonStyle::Ghost;

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
    let page_nav = nav.clone();
    show_glyph_dialog(
        nav,
        move || {
            let cancel_nav = page_nav.clone();
            let confirm_nav = page_nav.clone();
            glyph_dialog()
                .title(title.clone())
                .body(message.clone())
                .action(
                    button(CANCEL_LABEL, move |_state: &mut State| {
                        cancel_nav.pop();
                    })
                    .style(ButtonStyle::Ghost)
                    .small(),
                )
                .action(
                    button(confirm_label.clone(), move |_state: &mut State| {
                        confirm_nav.pop_with_result(PopResult::of(true));
                    })
                    .style(ButtonStyle::Danger)
                    .small(),
                )
        },
        move |state: &mut State, result: PopResult| {
            // Only the confirm action pops with a payload; every dismissal pops empty.
            if result.take::<bool>().unwrap_or(false) {
                on_confirm(state);
            }
        },
    );
}

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
            // Tracked read: a keystroke's `value.set` rebuilds this page.
            let current = value.get();
            let can_submit = submittable(&current);
            let enter_nav = page_nav.clone();
            let submit_nav = page_nav.clone();
            let cancel_nav = page_nav.clone();
            let field = text_input(current.clone(), move |_state: &mut State, next: String| {
                value.set(next)
            })
            .on_submit(move |_state: &mut State, typed: String| submit(&enter_nav, &typed));
            glyph_dialog()
                .title(title.clone())
                .content(field)
                .action(
                    button(CANCEL_LABEL, move |_state: &mut State| cancel_nav.pop())
                        .style(ButtonStyle::Ghost)
                        .small(),
                )
                .action(
                    button(SUBMIT_LABEL, move |_state: &mut State| {
                        // The style is disabled's visual half; this guard is the behavioural one.
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
                )
        },
        move |state: &mut State, result: PopResult| {
            if let Some(name) = result.take::<String>() {
                on_submit(state, name);
            }
        },
    );
}

fn submittable(text: &str) -> bool {
    !text.trim().is_empty()
}

/// The one gate both the Enter key and the submit button route through.
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
        // The disabled look only reads as disabled while it differs from the enabled one.
        assert_ne!(SUBMIT_ENABLED_STYLE, SUBMIT_DISABLED_STYLE);
    }
}
