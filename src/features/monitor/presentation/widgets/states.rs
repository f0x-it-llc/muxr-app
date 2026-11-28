//! The full-page states that need no chart, and the dimming wash over a stale body.

use frust::motion::AnimatedOpacity;
use frust::{
    Align, Alignment, Axis, CrossAxisAlignment, EdgeInsets, FlexView, GestureDetector, Padding,
    SizedBox, Stack, View, any, icons, inflexible, text,
};
use frust_glyph::empty_state;

use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

pub const UNSUPPORTED_TITLE: &str = "This server can't report vitals yet";
pub const DISABLED_TITLE: &str = "Monitoring is off on this server";
pub const DISABLED_BODY: &str = "Monitoring is off by default. Turn it on in muxrctl on the server, or in muxrd's config, to see vitals here.";
pub const OFFLINE_TITLE: &str = "Not connected to this server";
const OFFLINE_BODY: &str = "Vitals return once the connection is back.";
const OFFLINE_GLYPH: &str = "\u{25cb}";
const STALE_OPACITY: f64 = 0.45;
const PLACEHOLDER_PAD: f64 = spacing::XL;

pub fn unsupported_body(host: &str, server_version: &str) -> String {
    let runs = if server_version.is_empty() {
        format!("{host} runs a muxrd")
    } else {
        format!("{host} runs muxrd {server_version}")
    };
    format!(
        "{runs}, which has no monitoring. Update muxrd on the server to see vitals here. \
         Sessions and terminals keep working as before."
    )
}

pub fn unsupported_state<S: 'static>(host: &str, server_version: &str) -> impl View<S> + use<S> {
    empty_state(UNSUPPORTED_TITLE, unsupported_body(host, server_version)).icon(icons::ARROW_UPWARD)
}

pub fn disabled_state<S: 'static>() -> impl View<S> + use<S> {
    empty_state(DISABLED_TITLE, DISABLED_BODY).icon(icons::POWER)
}

pub fn offline_state<S: 'static>() -> impl View<S> + use<S> {
    empty_state(OFFLINE_TITLE, OFFLINE_BODY).glyph(OFFLINE_GLYPH)
}

/// Stays mounted so the last sample remains readable; the absorber keeps taps off it.
pub fn dimmed<S: 'static>(body: impl View<S>, stale: bool) -> impl View<S> {
    let target = if stale { STALE_OPACITY } else { 1.0 };
    let absorber = if stale {
        any(GestureDetector(SizedBox::<S>(Some(f64::INFINITY), Some(f64::INFINITY))).on_tap(|_| {}))
    } else {
        any(SizedBox::<S>(Some(0.0), Some(0.0)))
    };
    Stack(vec![any(AnimatedOpacity(target, body)), absorber])
}

pub fn placeholder<S: 'static>(label: &str, colors: &Palette) -> impl View<S> + use<S> {
    let theme = active_theme();
    Align(
        Alignment::CENTER,
        Padding(
            EdgeInsets::all(PLACEHOLDER_PAD),
            FlexView::new(
                Axis::Vertical,
                vec![inflexible(
                    text(label.to_string())
                        .style(inked(&theme.type_scale.body_small, colors.fg_dim)),
                )],
            )
            .cross_axis(CrossAxisAlignment::Center),
        ),
    )
}
