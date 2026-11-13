//! The non-live card faces, as the Penpot "Components" frame draws them:
//!
//! - cached — when the listing filed no page state (the shell otherwise
//!   composes the Sessions snapshot instead), a dimmed list of the sessions the server last showed, a hollow
//!   heartbeat and `last seen …`;
//! - never / locked / failed — a centred vector glyph over two lines.
//!
//! The live face is **not** drawn here: the switcher component scales the
//! real shell page into that card, so [`server_face`] answers an empty box for
//! it. Vectors only; colours from the [`Palette`].

use std::time::Duration;

use frust::authoring::text::{TextAlign, TextOverflow};
use frust::{
    Align, Alignment, AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView, Padding,
    SizedBox, Theme, any, container, flexible, icon, icons, inflexible, text,
};

use crate::features::servers::domain::entities::{FaceKind, ServerFace, SessionGlimpse};
use crate::ui::paint::{HAIRLINE, RADIUS_FULL};
use crate::ui::palette::{Palette, inked};
use crate::ui::recency;
use crate::ui::spacing;

/// The centred glyph's size.
const GLYPH: f64 = 40.0;
/// A list row's status dot.
const ROW_DOT: f64 = 8.0;
/// The heartbeat's hollow ring.
const HEARTBEAT: f64 = 10.0;
/// The ring's stroke, a touch over the hairline so it reads when scaled down.
const RING_STROKE: f64 = 1.5;
/// Rows a cached face lists before it stops.
const MAX_ROWS: usize = 5;

/// A vertical stack of flex children.
fn vcolumn<S: 'static>(children: Vec<FlexChild<S>>) -> FlexView<S> {
    FlexView::new(Axis::Vertical, children)
}

/// `Never`'s two lines.
pub const NEVER_TITLE: &str = "not connected yet";
pub const NEVER_HINT: &str = "tap to connect";
/// `Locked`'s two lines.
pub const LOCKED_TITLE: &str = "locked";
pub const LOCKED_HINT: &str = "tap to unlock";
/// `Failed`'s second line (the first is the failure message).
pub const FAILED_HINT: &str = "tap to retry";

/// `last seen …`, `now_ms` being the clock read once for the strip.
pub fn last_seen_label(last_seen_ms: i64, now_ms: i64) -> String {
    let elapsed = (now_ms - last_seen_ms).max(0);
    let age = recency::age_label(Duration::from_millis(elapsed.unsigned_abs()));
    if age == "just now" {
        "last seen just now".to_string()
    } else {
        format!("last seen {age}")
    }
}

/// `face`'s card content. [`FaceKind::Live`] is an empty box (see the
/// [module docs](self)).
pub fn server_face<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    face: &ServerFace,
    now_ms: i64,
) -> AnyView<S> {
    match &face.kind {
        FaceKind::Live => any(SizedBox::<S>(None, None)),
        FaceKind::Cached {
            sessions,
            last_seen_ms,
            ..
        } => any(cached_face(theme, colors, sessions, *last_seen_ms, now_ms)),
        FaceKind::Never => any(glyph_face(
            theme,
            colors,
            icons::PLUG,
            colors.fg_dim,
            NEVER_TITLE,
            NEVER_HINT,
        )),
        FaceKind::Locked => any(glyph_face(
            theme,
            colors,
            icons::LOCK,
            colors.fg_dim,
            LOCKED_TITLE,
            LOCKED_HINT,
        )),
        FaceKind::Failed { message } => any(glyph_face(
            theme,
            colors,
            icons::REFRESH,
            colors.error,
            message,
            FAILED_HINT,
        )),
    }
}

/// A centred glyph over a title and a hint.
fn glyph_face<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    mark: frust::IconSource,
    tint: frust::authoring::Color,
    title: &str,
    hint: &str,
) -> impl frust::View<S> + use<S> {
    let lines = vcolumn(vec![
        inflexible(icon(mark).size(GLYPH).color(tint)),
        inflexible(SizedBox::<S>(None, Some(spacing::BASE))),
        inflexible(
            text(title.to_string())
                .style(inked(&theme.type_scale.body_medium, colors.fg_muted))
                .align(TextAlign::Center)
                .max_lines(2)
                .overflow(TextOverflow::Ellipsis),
        ),
        inflexible(SizedBox::<S>(None, Some(spacing::TIGHT))),
        inflexible(
            text(hint.to_string())
                .style(inked(&theme.type_scale.label_small, colors.fg_dim))
                .align(TextAlign::Center),
        ),
    ])
    .cross_axis(CrossAxisAlignment::Center);
    Align(
        Alignment::CENTER,
        Padding(EdgeInsets::symmetric(spacing::XL, spacing::XL), lines),
    )
}

/// The dimmed session list closed by the hollow heartbeat and `last seen`.
fn cached_face<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    sessions: &[SessionGlimpse],
    last_seen_ms: i64,
    now_ms: i64,
) -> impl frust::View<S> + use<S> {
    let mut rows: Vec<FlexChild<S>> = sessions
        .iter()
        .take(MAX_ROWS)
        .map(|glimpse| inflexible(glimpse_row(theme, colors, glimpse)))
        .collect();
    rows.push(flexible(1, SizedBox::<S>(None, None)));
    rows.push(inflexible(heartbeat_footer(
        theme,
        colors,
        last_seen_ms,
        now_ms,
    )));
    Padding(
        EdgeInsets::symmetric(spacing::XL, spacing::XL),
        vcolumn(rows).cross_axis(CrossAxisAlignment::Stretch),
    )
}

/// `dot  name` over the dim meta line, both dimmed — the server is not live.
fn glimpse_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    glimpse: &SessionGlimpse,
) -> impl frust::View<S> + use<S> {
    let dot = if glimpse.running {
        colors.success.with_alpha(0.6)
    } else {
        colors.fg_faintest
    };
    let lines = vcolumn(vec![
        inflexible(
            text(glimpse.name.clone())
                .style(inked(&theme.type_scale.body_small, colors.fg_muted))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
        inflexible(SizedBox::<S>(None, Some(spacing::MICRO))),
        inflexible(
            text(format!("{} · {}", glimpse.backend, glimpse.meta_line))
                .style(inked(&theme.type_scale.label_small, colors.fg_dim))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
    ])
    .cross_axis(CrossAxisAlignment::Stretch);
    Padding(
        EdgeInsets::symmetric(0.0, spacing::SM),
        FlexView::new(
            Axis::Horizontal,
            vec![
                inflexible(
                    container(SizedBox::<S>(Some(ROW_DOT), Some(ROW_DOT)))
                        .fill(dot)
                        .radius(RADIUS_FULL),
                ),
                inflexible(SizedBox::<S>(Some(spacing::MD), None)),
                flexible(1, lines),
            ],
        )
        .cross_axis(CrossAxisAlignment::Center),
    )
}

/// The hollow heartbeat ring and the `last seen …` line.
fn heartbeat_footer<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    last_seen_ms: i64,
    now_ms: i64,
) -> impl frust::View<S> + use<S> {
    let ring = container(SizedBox::<S>(Some(HEARTBEAT), Some(HEARTBEAT)))
        .border(colors.fg_dim, RING_STROKE.max(HAIRLINE))
        .radius(RADIUS_FULL);
    FlexView::new(
        Axis::Horizontal,
        vec![
            inflexible(ring),
            inflexible(SizedBox::<S>(Some(spacing::SM), None)),
            inflexible(
                text(last_seen_label(last_seen_ms, now_ms))
                    .style(inked(&theme.type_scale.label_small, colors.fg_dim)),
            ),
        ],
    )
    .cross_axis(CrossAxisAlignment::Center)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_seen_reads_the_apps_recency_wording() {
        const MIN: i64 = 60_000;
        assert_eq!(last_seen_label(1_000, 1_000), "last seen just now");
        assert_eq!(last_seen_label(0, 5 * MIN), "last seen 5 minutes ago");
        assert_eq!(
            last_seen_label(10 * MIN, 0),
            "last seen just now",
            "a future stamp never reads negative"
        );
    }
}
