//! Pieces 3 and 4: the two `.field`s.

use std::sync::Arc;

use frust::authoring::Color;
use frust::authoring::text::{FamilyName, FontFamily, GenericSlot, TextStyle};
use frust::{Get, Theme, View, any, text, text_input};

use super::{Live, backed, block_column, v_gap};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

/// `pub`: the exact string `tests/connect.rs`'s shaping suite measures.
pub const SERVER_URL_PLACEHOLDER: &str = "zelli.example.com:50051";
/// All letters: the control string the device rendered correctly.
pub const TOKEN_PLACEHOLDER: &str = "paste or scan a token";

/// The label inks `error` while `invalid` is set: `TextInput` exposes no error state.
pub fn server_url_field<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> impl View<S> {
    let invalid = live.is_some_and(|live| live.controller.invalid.get());
    let value = live
        .map(|live| live.controller.url.get())
        .unwrap_or_default();
    let error = live.and_then(|live| live.controller.url_error.get());
    let controller = live.map(|live| Arc::clone(&live.controller));
    field(
        theme,
        colors,
        "SERVER URL",
        SERVER_URL_PLACEHOLDER,
        value,
        FieldOptions {
            invalid,
            error,
            obscured: false,
            read_only: live.is_none(),
        },
        move |next| {
            if let Some(controller) = &controller {
                controller.set_url(next);
            }
        },
    )
}

/// Obscuring hides the rendered glyphs, not the platform IME's suggestion strip, which
/// still surfaces the text as it is typed.
pub fn token_field<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> impl View<S> {
    let value = live
        .map(|live| live.controller.token.get().as_str().to_owned())
        .unwrap_or_default();
    let controller = live.map(|live| Arc::clone(&live.controller));
    field(
        theme,
        colors,
        "LOGIN TOKEN",
        TOKEN_PLACEHOLDER,
        value,
        FieldOptions {
            invalid: false,
            error: None,
            obscured: true,
            read_only: live.is_none(),
        },
        move |next| {
            if let Some(controller) = &controller {
                controller.set_token(next);
            }
        },
    )
}

/// One struct keeps [`field`] under clippy's `too_many_arguments` threshold.
struct FieldOptions {
    invalid: bool,
    error: Option<String>,
    obscured: bool,
    /// Refuses focus and editing at full alpha; `enabled(false)` would also dim.
    read_only: bool,
}

/// `TextInputWidget` bakes its padding, border width and focus treatment into private
/// constants; only `text_style` is reachable from the app.
fn field<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    label: &str,
    placeholder: &str,
    value: String,
    options: FieldOptions,
    on_change: impl Fn(String) + 'static,
) -> impl View<S> {
    let label_ink = if options.invalid {
        colors.error
    } else {
        colors.fg_dim
    };
    let mut children = vec![
        any(text(label).style(inked(&theme.type_scale.label_small, label_ink))),
        any(v_gap::<S>(spacing::XS)),
        any(
            text_input::<S, _>(value, move |_state: &mut S, next: String| on_change(next))
                .placeholder(placeholder)
                .text_style(field_text_style(theme, colors.fg))
                .obscured(options.obscured)
                .read_only(options.read_only),
        ),
    ];
    if let Some(message) = options.error {
        children.push(any(v_gap::<S>(spacing::XS)));
        children.push(any(
            text(message).style(inked(&theme.type_scale.body_small, colors.error))
        ));
    }
    backed(block_column::<S, _>(children), colors)
}

/// `pub` so `tests/connect.rs`'s shaping suite uses the real style, not a copy.
pub fn field_text_style(theme: &Theme, ink: Color) -> TextStyle {
    let mut style = inked(&theme.type_scale.body_medium, ink);
    style.family = field_font_family(&style.family);
    style
}

/// Appends a generic that resolves everywhere: `TextInputWidget` owns a private
/// `TextContext`, so the bundled faces never reach a field and `monospace` resolves to
/// nothing on Android, where parley then advances every digit a full em.
fn field_font_family(family: &FontFamily) -> FontFamily {
    let mut names: Vec<FamilyName> = match family {
        FontFamily::SystemUi => vec![FamilyName::Generic(GenericSlot::SystemUi)],
        FontFamily::Named(names) => names.iter().cloned().map(FamilyName::Named).collect(),
        FontFamily::NamedWithGeneric(names) => names.clone(),
    };
    names.push(FamilyName::Generic(GenericSlot::SansSerif));
    FontFamily::NamedWithGeneric(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fields_font_stack_always_ends_in_a_resolvable_generic() {
        let theme = frust_glyph::baseline();
        let style = field_text_style(&theme, Color::WHITE);
        let FontFamily::NamedWithGeneric(names) = &style.family else {
            panic!("a field's family must be a stack ending in a generic slot");
        };
        assert_eq!(
            names.last(),
            Some(&FamilyName::Generic(GenericSlot::SansSerif)),
            "got {names:?}"
        );
        assert!(
            names.contains(&FamilyName::Named("IBM Plex Mono".to_string())),
            "the bundled Glyph face is still preferred: {names:?}"
        );
        assert!(
            names.contains(&FamilyName::Generic(GenericSlot::Monospace)),
            "a platform monospace is still tried before the rescue: {names:?}"
        );
    }
}
