//! The row above a card: a status dot (or the lock for a locked server), the
//! host, and the trust badge.

use frust::{
    Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView, Padding, SizedBox, Theme, View, any,
    container, icon, icons, inflexible, text,
};
use frust_glyph::{BadgeVariant, badge};

use crate::connection::TrustMode;
use crate::features::servers::domain::entities::{FaceKind, ServerFace};
use crate::ui::paint::RADIUS_FULL;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

const DOT: f64 = 8.0;
const LOCK_MARK: f64 = 12.0;

pub fn trust_badge_label(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::System => "system TLS",
        TrustMode::NoVerification => "no-verification TLS",
        TrustMode::Pinned => "pinned TLS",
    }
}

/// `dot/lock  host  [trust]`, tinted by the face's kind: live success, failed
/// error, everything else dim.
pub fn label_row<S: 'static>(theme: &Theme, colors: &Palette, face: &ServerFace) -> impl View<S> {
    let mark = match &face.kind {
        FaceKind::Locked => any(icon(icons::LOCK).size(LOCK_MARK).color(colors.fg_dim)),
        kind => {
            let tint = match kind {
                FaceKind::Live => colors.success,
                FaceKind::Failed { .. } => colors.error,
                _ => colors.fg_dim,
            };
            any(container(SizedBox::<S>(Some(DOT), Some(DOT)))
                .fill(tint)
                .radius(RADIUS_FULL))
        }
    };
    let ink = if face.kind == FaceKind::Live {
        colors.fg
    } else {
        colors.fg_muted
    };
    let children: Vec<FlexChild<S>> = vec![
        inflexible(mark),
        inflexible(SizedBox::<S>(Some(spacing::SM), None)),
        inflexible(text(face.label.clone()).style(inked(&theme.type_scale.label_large, ink))),
        inflexible(SizedBox::<S>(Some(spacing::SM), None)),
        inflexible(badge(
            trust_badge_label(face.trust_mode),
            BadgeVariant::Neutral,
        )),
    ];
    Padding(
        EdgeInsets::symmetric(0.0, spacing::TIGHT),
        FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center),
    )
}
