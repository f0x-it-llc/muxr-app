//! Piece 7 — `.saved-hdr` + `.saved-row`: the saved-server list, and the
//! snapshot of it the splash renders.

use std::rc::Rc;

use frust::authoring::Size;
use frust::{
    Align, Alignment, AnyView, EdgeInsets, FlexChild, GestureDetector, Padding, SizedBox, Theme,
    any, colored_box, container, flexible, icon, icons, inflexible, text,
};
use frust_glyph::{BadgeVariant, avatar, badge};

use super::pairing::READ_ONLY_CHIP;
use super::{Live, backed, column, h_gap, row, v_gap};
use crate::connection::TrustMode;
use crate::features::connect::di::saved_servers;
use crate::features::connect::domain::entities::{SavedServer, epoch_millis};
use crate::ui::paint::{GLOW_SPREAD, GLOW_STD_DEV, HAIRLINE};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;
use crate::ui::topbar::icon_button;

const DOT: f64 = 7.0;
const SAVED_ROW_PAD_Y: f64 = 11.0;
/// The rule's weight.
const RULE_HEIGHT: f64 = 1.0;
/// A saved row's delete affordance press target, at the platform minimum tap target.
const DELETE_TARGET: f64 = 44.0;
/// The mark inside it, smaller than the icon's own 24px design box: quiet
/// chrome beside the row's content, not a second action button.
const DELETE_MARK: f64 = 18.0;

/// The chip a saved row carries when a gated credential is stored for it — also
/// what says a tap will raise a prompt rather than refill the form.
pub const CREDENTIAL_CHIP: &str = "credential";
/// What the Tablet list says in place of rows when nothing is saved.
pub const NO_SAVED_SERVERS: &str = "no saved servers";
/// The Tablet row's leading initial box.
const AVATAR: f64 = 26.0;

/// Piece 7 — the saved-server list, each row tappable to refill the form.
/// `servers` is handed down rather than read here (the parity contract); an
/// empty list renders the header alone on both sides.
pub fn saved_address<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    servers: &[SavedServer],
    live: Option<&Live>,
) -> AnyView<S> {
    // One clock read for the whole list — every row's recency measured against it.
    let now = epoch_millis();
    let mut children = vec![saved_header(theme, colors), v_gap(spacing::MICRO)];
    for (index, server) in servers.iter().enumerate() {
        children.push(saved_row(theme, colors, server, index, now, live));
    }
    backed(column::<S>(children), colors)
}

/// An uppercase micro label followed by a hairline rule filling the rest of the row.
fn saved_header<S: 'static>(theme: &Theme, colors: &Palette) -> AnyView<S> {
    any(row::<S>(vec![
        inflexible(any(
            text("SAVED SERVERS").style(inked(&theme.type_scale.label_small, colors.fg_dim))
        )),
        inflexible(h_gap::<S>(spacing::SM)),
        // A zero-width box under a flexible slot gets the remaining width.
        flexible(
            1,
            any(colored_box::<S>().size(0.0, RULE_HEIGHT).fill(colors.rule)),
        ),
    ]))
}

/// This mode's short form, for a saved row's meta line.
fn trust_meta_label(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::System => "system TLS",
        // Not "first-use TLS": that name claimed a mechanism this posture never implemented.
        TrustMode::NoVerification => "no-verification TLS",
        TrustMode::Pinned => "pinned TLS",
    }
}

/// Status dot, `host:port` over its meta line, the credential and read-only
/// badges, and the trailing delete affordance. Tapping a row with a stored
/// credential unlocks and reconnects rather than refilling the form.
///
/// The delete affordance sits beside the row's own tap recognizer, not
/// inside it: a nested press target would fire the row's tap regardless
/// (that recognizer captures on `Down`, fires on its own `Up`). Siblings are
/// disjoint by construction.
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
        // The glow is part of the dot, not decoration — it's what reads as "live" at 7px.
        inflexible(any(colored_box::<S>()
            .size(DOT, DOT)
            .radius(DOT / 2.0)
            .fill(colors.success)
            .glow(colors.success, GLOW_STD_DEV, GLOW_SPREAD))),
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
    items.extend(chips::<S>(server));
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

/// The credential and read-only chips a row carries, each led by its gap.
fn chips<S: 'static>(server: &SavedServer) -> Vec<FlexChild<S>> {
    let mut items = Vec::new();
    for (shown, chip) in [
        (server.has_credential, CREDENTIAL_CHIP),
        (server.read_only, READ_ONLY_CHIP),
    ] {
        if shown {
            items.push(inflexible(h_gap::<S>(spacing::MD)));
            items.push(inflexible(any(badge(chip, BadgeVariant::Neutral))));
        }
    }
    items
}

/// The Tablet variant of piece 7: the same header over card rows, or a muted
/// line when nothing is saved. Tap, delete and chips wire exactly as the
/// phone row's do.
pub fn tablet_saved_list<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    servers: &[SavedServer],
    live: &Live,
) -> AnyView<S> {
    let now = epoch_millis();
    let mut children = vec![saved_header(theme, colors), v_gap(spacing::MD)];
    if servers.is_empty() {
        children.push(any(
            text(NO_SAVED_SERVERS).style(inked(&theme.type_scale.body_small, colors.fg_muted))
        ));
    }
    for (index, server) in servers.iter().enumerate() {
        if index > 0 {
            children.push(v_gap(spacing::SM));
        }
        children.push(tablet_saved_row(theme, colors, server, index, now, live));
    }
    any(column::<S>(children))
}

/// A bordered card row: the label's initial, the label over
/// `host:port · trust · recency`, the chips, and the delete target.
fn tablet_saved_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    server: &SavedServer,
    index: usize,
    now_ms: i64,
    live: &Live,
) -> AnyView<S> {
    let meta = format!(
        "{} · {} · {}",
        server.endpoint(),
        trust_meta_label(server.trust_mode),
        server.last_used_label(now_ms)
    );
    let initial: String = server
        .label
        .chars()
        .chain(server.host.chars())
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    let mut items = vec![
        // The label beside it already names the row.
        inflexible(any(avatar(initial).size(AVATAR).decorative())),
        inflexible(h_gap::<S>(spacing::MD)),
        flexible(
            1,
            any(column::<S>(vec![
                any(text(server.label.clone())
                    .style(inked(&theme.type_scale.body_small, colors.fg))),
                v_gap::<S>(spacing::MICRO),
                any(text(meta).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
            ])),
        ),
    ];
    items.extend(chips::<S>(server));
    let on_tap = Rc::clone(&live.on_saved_tap);
    let body = GestureDetector(row::<S>(items)).on_tap(move |_state: &mut S| on_tap(index));
    let content = row::<S>(vec![
        flexible(1, any(body)),
        inflexible(h_gap::<S>(spacing::MD)),
        inflexible(delete_affordance(colors, server, index, Some(live))),
    ]);
    let inset = EdgeInsets {
        left: spacing::BASE,
        top: spacing::XS,
        right: spacing::TIGHT,
        bottom: spacing::XS,
    };
    any(container(Padding(inset, content))
        .fill(colors.surface)
        .radius(theme.shape.medium)
        .border(colors.outline, HAIRLINE))
}

/// A dim [`icons::DELETE`] mark in a [`DELETE_TARGET`]-square press target,
/// dropping this device's copy of the server it sits on. `live` is `None`
/// for the splash, which paints the same mark with no recognizer and no
/// `Role::Button` node.
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
    // A labelled `Role::Button`, which a bare `GestureDetector` could not give a screen reader.
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

/// The delete affordance's accessible name — a bare "Delete" would announce
/// a list of identical controls.
fn delete_label(label: &str) -> String {
    format!("Delete {label}")
}

/// The saved-server rows the splash's copy of piece 7 renders.
///
/// Read once per launch by the composition root and carried through every
/// rebuild, since the real store is a preferences read plus a JSON parse.
/// Both ways this could fail — an unpublished repository, a failing
/// `list()` — answer with no rows rather than a panic or a toast; the live
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
