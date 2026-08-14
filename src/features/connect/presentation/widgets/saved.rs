//! Piece 7 — `.saved-hdr` + `.saved-row`: the saved-server list, and the
//! snapshot of it the splash renders.

use std::rc::Rc;

use frust::authoring::Size;
use frust::glyph::{BadgeVariant, badge};
use frust::{
    Align, Alignment, AnyView, EdgeInsets, GestureDetector, Padding, SizedBox, Theme, any,
    flexible, icon, icons, inflexible, text,
};

use super::super::chrome::swatch;
use super::pairing::READ_ONLY_CHIP;
use super::{Live, backed, column, h_gap, row, v_gap};
use crate::connection::TrustMode;
use crate::features::connect::di::saved_servers;
use crate::features::connect::domain::entities::{SavedServer, epoch_millis};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;
use crate::ui::topbar::icon_button;

/// `.dot-ok{width:7px;height:7px}`.
const DOT: f64 = 7.0;
/// `.saved-row{padding:11px 0}`.
const SAVED_ROW_PAD_Y: f64 = 11.0;
/// `.saved-hdr::after{height:1px}` — the rule's weight.
const RULE_HEIGHT: f64 = 1.0;
/// A saved row's delete affordance: its press target, at the platform minimum tap
/// target. The mock draws no such affordance.
const DELETE_TARGET: f64 = 44.0;
/// …and the mark inside it, smaller than the icon's own 24px design box: quiet
/// chrome beside the row's content, not a second action button.
const DELETE_MARK: f64 = 18.0;

/// The chip a saved row carries when a gated credential is stored for it — also
/// what says a tap will raise a prompt rather than refill the form.
pub const CREDENTIAL_CHIP: &str = "credential";

/// Piece 7 — the saved-server list, each row tappable to refill the form.
/// `servers` is handed down rather than read here, which is what makes the
/// screen's and the splash's pictures identical (the parity contract); an empty
/// list renders the header alone on **both** sides.
pub fn saved_address<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    servers: &[SavedServer],
    live: Option<&Live>,
) -> AnyView<S> {
    // One clock read for the whole list: every row's recency is measured against
    // the same instant, and this piece rebuilds at signal rate.
    let now = epoch_millis();
    let mut children = vec![saved_header(theme, colors), v_gap(spacing::MICRO)];
    for (index, server) in servers.iter().enumerate() {
        children.push(saved_row(theme, colors, server, index, now, live));
    }
    backed(column::<S>(children), colors)
}

/// `.saved-hdr`: an uppercase micro label followed by a hairline rule filling
/// the rest of the row (`::after{flex:1;height:1px}`).
fn saved_header<S: 'static>(theme: &Theme, colors: &Palette) -> AnyView<S> {
    any(row::<S>(vec![
        inflexible(any(
            text("SAVED SERVERS").style(inked(&theme.type_scale.label_small, colors.fg_dim))
        )),
        inflexible(h_gap::<S>(spacing::SM)),
        // A zero-width swatch under a flexible slot: the flex's tight
        // cross-constraint gives it the remaining width.
        flexible(
            1,
            any(swatch(Size::new(0.0, RULE_HEIGHT), colors.rule, 0.0)),
        ),
    ]))
}

/// This mode's short form, for a saved row's meta line (the mock's
/// "insecure-dev TLS · port 50051").
fn trust_meta_label(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::System => "system TLS",
        // Not "first-use TLS": that name claimed a trust-on-first-use mechanism
        // this posture never implemented.
        TrustMode::NoVerification => "no-verification TLS",
        TrustMode::Pinned => "pinned TLS",
    }
}

/// One `.saved-row`: status dot, `host:port` over its meta line, the credential and
/// read-only badges, and the trailing delete affordance. Tapping a row with a
/// stored credential unlocks and reconnects rather than refilling the form, which
/// is why the badge matters.
///
/// The delete affordance sits **beside** the row's own tap recognizer, not inside
/// it: a nested press target would be pressed *and* still fire the row's tap, since
/// that recognizer captures on `Down` and fires on its own `Up` regardless of what
/// a child did. Siblings are disjoint by construction.
fn saved_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    server: &SavedServer,
    index: usize,
    now_ms: i64,
    live: Option<&Live>,
) -> AnyView<S> {
    let meta = format!(
        "{} · {} · port {}",
        trust_meta_label(server.trust_mode),
        server.last_used_label(now_ms),
        server.port
    );
    let mut items = vec![
        // The glow is part of the dot, not decoration: it is what reads as "live"
        // at 7px.
        inflexible(any(
            swatch(Size::new(DOT, DOT), colors.success, DOT / 2.0).glow(colors.success)
        )),
        inflexible(h_gap::<S>(spacing::MD)),
        flexible(
            1,
            any(column::<S>(vec![
                any(text(server.endpoint()).style(inked(&theme.type_scale.body_small, colors.fg))),
                v_gap::<S>(spacing::MICRO),
                any(text(meta).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
            ])),
        ),
    ];
    if server.has_credential {
        items.push(inflexible(h_gap::<S>(spacing::MD)));
        items.push(inflexible(any(badge(
            CREDENTIAL_CHIP,
            BadgeVariant::Neutral,
        ))));
    }
    if server.read_only {
        items.push(inflexible(h_gap::<S>(spacing::MD)));
        items.push(inflexible(any(badge(
            READ_ONLY_CHIP,
            BadgeVariant::Neutral,
        ))));
    }
    let body = Padding(EdgeInsets::symmetric(0.0, SAVED_ROW_PAD_Y), row::<S>(items));
    let tappable: AnyView<S> = match live {
        None => any(body),
        Some(live) => {
            let on_tap = Rc::clone(&live.on_saved_tap);
            any(GestureDetector(body).on_tap(move |_state: &mut S| on_tap(index)))
        }
    };
    any(row::<S>(vec![
        flexible(1, tappable),
        inflexible(h_gap::<S>(spacing::MD)),
        inflexible(delete_affordance(colors, server, index, live)),
    ]))
}

/// A saved row's trailing delete affordance: a dim [`icons::DELETE`] mark in a
/// [`DELETE_TARGET`]-square press target, dropping **this device's** copy of the
/// server it sits on. Quiet on purpose, since every row carries one. `live` is
/// `None` for the splash, which paints the same mark at the same metrics with no
/// recognizer and no `Role::Button` node.
fn delete_affordance<S: 'static>(
    colors: &Palette,
    server: &SavedServer,
    index: usize,
    live: Option<&Live>,
) -> AnyView<S> {
    let Some(live) = live else {
        return any(
            SizedBox::<S>(Some(DELETE_TARGET), Some(DELETE_TARGET)).child(Align(
                Alignment::CENTER,
                icon(icons::DELETE).size(DELETE_MARK).color(colors.fg_dim),
            )),
        );
    };
    let on_delete = Rc::clone(&live.on_saved_delete);
    // A *labelled* `Role::Button` over one vector, which a bare `GestureDetector`
    // could not give a screen reader.
    any(icon_button(
        icons::DELETE,
        delete_label(&server.label),
        move |_state: &mut S| on_delete(index),
    )
    .ink(colors.fg_dim)
    .icon_size(DELETE_MARK)
    .box_size(Size::new(DELETE_TARGET, DELETE_TARGET))
    .radius(DELETE_TARGET / 2.0))
}

/// The delete affordance's accessible name. The **server** is what a screen reader
/// has to hear: a bare "Delete" would announce a list of identical controls.
fn delete_label(label: &str) -> String {
    format!("Delete {label}")
}

/// The saved-server rows the splash's copy of piece 7 renders, exactly as
/// `SavedServersRepository::list` answers them.
///
/// Read **once per launch** by the composition root and carried through every
/// rebuild, since the shell rebuilds the whole tree every frame and the real store
/// is a preferences read plus a JSON parse. A snapshot is also all the splash can
/// honestly show. Both ways this could fail — an unpublished repository, a failing
/// `list()` — answer with **no rows** rather than a panic or a toast; the live
/// screen re-reads the same store a moment later.
pub fn splash_saved_servers() -> Vec<SavedServer> {
    let Some(repository) = saved_servers() else {
        log::warn!("splash: no saved-server repository published yet — assembling with no rows");
        return Vec::new();
    };
    repository.list().unwrap_or_else(|failure| {
        log::warn!("splash: listing saved servers failed ({failure:?}) — assembling with no rows");
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every posture, for the label-uniqueness sweep below.
    const ALL_TRUST_MODES: [TrustMode; 3] = [
        TrustMode::System,
        TrustMode::NoVerification,
        TrustMode::Pinned,
    ];

    #[test]
    fn every_trust_mode_has_its_own_meta_label() {
        let mut metas: Vec<&str> = ALL_TRUST_MODES
            .iter()
            .copied()
            .map(trust_meta_label)
            .collect();
        metas.sort_unstable();
        metas.dedup();
        assert_eq!(metas.len(), ALL_TRUST_MODES.len());
    }
}
