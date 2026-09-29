//! Pieces 3 and 4 — the two `.field`s: the server URL and the login token.

use std::sync::Arc;

use frust::authoring::Color;
use frust::authoring::text::{FamilyName, FontFamily, GenericSlot, TextStyle};
use frust::{AnyView, Get, Theme, any, text, text_input};

use super::{Live, backed, column, v_gap};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

/// The server-URL field's ghost text. `pub` because it is the exact string
/// `tests/connect.rs`'s shaping suite measures (see [`field_font_family`]).
pub const SERVER_URL_PLACEHOLDER: &str = "zelli.example.com:50051";
/// The token field's — the all-letter control the device rendered correctly.
pub const TOKEN_PLACEHOLDER: &str = "paste or scan a token";

/// Piece 3 — the server URL. Its label inks `error` while the controller's
/// `invalid` is set: the screen's whole validation surface, since `TextInput`
/// exposes no error state of its own. Read-only for the splash's static copy.
pub fn server_url_field<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> AnyView<S> {
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

/// Piece 4 — the login token, masked. Obscuring hides the rendered glyphs,
/// not the platform IME's own suggestion strip, which still surfaces the
/// text as it is typed (`docs/ARCHITECTURE.md`'s Planned section).
pub fn token_field<S: 'static>(theme: &Theme, colors: &Palette, live: Option<&Live>) -> AnyView<S> {
    // `TextInputView` takes a plain `String` by value; the same display
    // surface the masking note above covers, not a new leak.
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

/// Per-field flags [`field`] needs beyond its label/placeholder/value — one
/// struct so its parameter count stays under clippy's `too_many_arguments` threshold.
struct FieldOptions {
    /// Inks the label `error` while the form is invalid — only
    /// [`server_url_field`] ever sets this.
    invalid: bool,
    /// The helper line under the input, in the error tone — only
    /// [`server_url_field`] ever sets this.
    error: Option<String>,
    /// Mask the rendered glyphs (password mode) — only [`token_field`] sets it.
    obscured: bool,
    /// Refuses focus/caret/editing while painting at full alpha. `true` for
    /// the splash. Never `enabled(false)`, which also dims.
    read_only: bool,
}

/// A `.field`: an uppercase micro label over a placeholder-bearing input.
/// `on_change` is invoked with the requested text; a splash-composed field
/// passes one that drops it.
///
/// Chrome the app cannot reach: `TextInputWidget` bakes its padding, border
/// width and focus treatment into private constants and exposes only
/// `text_style` — the radius, fill and idle border are already right; only
/// the framework can close the rest.
fn field<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    label: &str,
    placeholder: &str,
    value: String,
    options: FieldOptions,
    on_change: impl Fn(String) + 'static,
) -> AnyView<S> {
    let label_ink = if options.invalid {
        colors.error
    } else {
        colors.fg_dim
    };
    let mut children = vec![
        any(text(label).style(inked(&theme.type_scale.label_small, label_ink))),
        v_gap::<S>(spacing::XS),
        any(
            text_input::<S, _>(value, move |_state: &mut S, next: String| on_change(next))
                .placeholder(placeholder)
                .text_style(field_text_style(theme, colors.fg))
                .obscured(options.obscured)
                .read_only(options.read_only),
        ),
    ];
    if let Some(message) = options.error {
        children.push(v_gap::<S>(spacing::XS));
        children.push(any(
            text(message).style(inked(&theme.type_scale.body_small, colors.error))
        ));
    }
    backed(column::<S>(children), colors)
}

/// `body_medium` re-inked, with one deliberate change to its family, applied
/// to both the field's content and its placeholder. `pub` so
/// `tests/connect.rs`'s shaping suite uses the real style, not a copy.
pub fn field_text_style(theme: &Theme, ink: Color) -> TextStyle {
    let mut style = inked(&theme.type_scale.body_medium, ink);
    style.family = field_font_family(&style.family);
    style
}

/// `family` with a resolvable terminal fallback appended
/// ([`GenericSlot::SansSerif`]) — the app-side workaround for a real defect:
/// digits render individually wide-spaced in this field.
///
/// Root cause: `TextInputWidget` owns a private `TextContext`, so the bundled
/// Glyph faces never reach a field, and fontique leaves
/// `GenericFamily::Monospace` empty on Android. The body role's
/// `["IBM Plex Mono"] + monospace` stack resolves to nothing there, so parley
/// falls back per cluster, advancing every digit a full em. A generic that
/// resolves everywhere collapses it back to one run. Scoped to the fields on
/// purpose — every other run on this screen shapes against the shell's
/// font-registered context and resolves mono correctly.
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
