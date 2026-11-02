//! Piece 8 — `.saved`: the saved-server list closed by the unlock dock, and
//! the snapshot of it the splash renders.
//!
//! The rows are informational: the dock unlocks and dials, so a row tap does
//! nothing — except where the device has no biometric gate, when the dock is
//! disabled and each row keeps its own tap-to-unlock.

use std::rc::Rc;

use frust::authoring::Size;
use frust::{
    Align, Alignment, EdgeInsets, Either, FlexChild, GestureDetector, Padding, SizedBox, Theme,
    View, any, colored_box, container, flexible, icon, icons, inflexible, text,
};
use frust_glyph::{BadgeVariant, avatar, badge};

use super::super::controller::DockState;
use super::keyring_dock::{DockActions, keyring_dock};
use super::pairing::READ_ONLY_CHIP;
use super::pro_gate::row_is_locked;
use super::{Live, backed, block_column, centered_row, h_gap, v_gap};
use crate::connection::TrustMode;
use crate::features::connect::di::saved_servers;
use crate::features::connect::domain::entities::{SavedServer, epoch_millis};
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;
use crate::ui::topbar::icon_button;

const SAVED_ROW_PAD_Y: f64 = 11.0;
/// The rule's weight.
const RULE_HEIGHT: f64 = 1.0;
/// A saved row's delete affordance press target, at the platform minimum tap target.
const DELETE_TARGET: f64 = 44.0;
/// The mark inside it, smaller than the icon's own 24px design box: quiet
/// chrome beside the row's content, not a second action button.
const DELETE_MARK: f64 = 18.0;

/// What the Tablet list says in place of rows when nothing is saved.
pub const NO_SAVED_SERVERS: &str = "no saved servers";
/// The tag a row past the free tier's limit carries.
pub const PRO_TAG: &str = "pro";
/// The lock mark leading every phone row: its credential sits in the keyring.
const LOCK_MARK: f64 = 12.0;
/// The Tablet row's leading initial box.
const AVATAR: f64 = 26.0;

/// Piece 8 — the saved-server list, then the dock when anything is saved.
/// `servers` is handed down rather than read here (the parity contract); an
/// empty list renders the header alone on both sides.
pub fn saved_address<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    servers: &[SavedServer],
    live: Option<&Live>,
) -> impl View<S> + use<S> {
    // One clock read for the whole list — every row's recency measured against it.
    let now = epoch_millis();
    // erasure: keep accumulator into Vec<AnyView>
    let mut children = vec![
        any(saved_header::<S>(theme, colors)),
        any(v_gap::<S>(spacing::MICRO)),
    ];
    for (index, server) in servers.iter().enumerate() {
        children.push(any(saved_row(
            theme, colors, server, servers, index, now, live,
        )));
    }
    if !servers.is_empty() {
        children.push(any(dock(theme, colors, servers, live)));
    }
    backed(block_column::<S, _>(children), colors)
}

/// The dock as `live` wires it — idle and unwired for the splash.
fn dock<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    servers: &[SavedServer],
    live: Option<&Live>,
) -> impl View<S> + use<S> {
    let state = live.map_or(DockState::Idle, |live| live.controller.dock_state());
    let actions = live.map(|live| DockActions {
        on_unlock: Rc::clone(&live.on_unlock),
        on_reset: Rc::clone(&live.on_reset_credentials),
    });
    keyring_dock(theme, colors, servers, &state, actions.as_ref())
}

/// An uppercase micro label followed by a hairline rule filling the rest of the row.
fn saved_header<S: 'static>(theme: &Theme, colors: &Palette) -> impl View<S> + use<S> {
    centered_row::<S>(vec![
        inflexible(
            text("SAVED SERVERS").style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        ),
        inflexible(h_gap::<S>(spacing::SM)),
        // A zero-width box under a flexible slot gets the remaining width.
        flexible(
            1,
            colored_box::<S>().size(0.0, RULE_HEIGHT).fill(colors.rule),
        ),
    ])
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

/// `body` behind the row's own tap, wired only where the dock cannot unlock
/// (see the [module docs](self)); the splash has none either.
fn tappable<S: 'static, V: View<S>>(
    body: V,
    live: Option<&Live>,
    index: usize,
) -> impl View<S> + use<S, V> {
    let row_tap = live
        .filter(|live| !live.controller.biometrics_available())
        .map(|live| Rc::clone(&live.on_saved_tap));
    match row_tap {
        None => Either::Left(body),
        Some(on_tap) => {
            Either::Right(GestureDetector(body).on_tap(move |_state: &mut S| on_tap(index)))
        }
    }
}

/// The lock mark, `host:port` over `trust · recency`, the read-only chip and
/// pro tag, and the trailing delete affordance.
///
/// The delete affordance sits beside the row's own (fallback) tap recognizer,
/// not inside it: a nested press target would fire the row's tap regardless
/// (that recognizer captures on `Down`, fires on its own `Up`). Siblings are
/// disjoint by construction.
fn saved_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    server: &SavedServer,
    servers: &[SavedServer],
    index: usize,
    now_ms: i64,
    live: Option<&Live>,
) -> impl View<S> + use<S> {
    let meta = format!(
        "{} · {}",
        trust_meta_label(server.trust_mode),
        server.last_used_label(now_ms),
    );
    let mut items = vec![
        inflexible(icon(icons::LOCK).size(LOCK_MARK).color(colors.fg_dim)),
        inflexible(h_gap::<S>(spacing::MD)),
        flexible(
            1,
            block_column::<S, _>((
                text(server.endpoint()).style(inked(&theme.type_scale.body_small, colors.fg)),
                v_gap::<S>(spacing::MICRO),
                text(meta).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
            )),
        ),
    ];
    items.extend(chips::<S>(server, locked(server, servers, live)));
    let body = Padding(
        EdgeInsets::symmetric(0.0, SAVED_ROW_PAD_Y),
        centered_row::<S>(items),
    );
    centered_row::<S>(vec![
        flexible(1, tappable(body, live, index)),
        inflexible(h_gap::<S>(spacing::MD)),
        inflexible(delete_affordance(colors, server, index, live)),
    ])
}

/// Whether the row wears the pro tag. The splash has no controller, so no
/// entitlement to judge by, and paints none.
fn locked(server: &SavedServer, servers: &[SavedServer], live: Option<&Live>) -> bool {
    live.is_some_and(|live| row_is_locked(server, servers, live.controller.entitlement()))
}

/// The read-only chip, then the pro tag when `locked`, each led by its gap.
fn chips<S: 'static>(server: &SavedServer, locked: bool) -> Vec<FlexChild<S>> {
    let mut items = Vec::new();
    for (shown, chip, variant) in [
        (server.read_only, READ_ONLY_CHIP, BadgeVariant::Neutral),
        (locked, PRO_TAG, BadgeVariant::Accent),
    ] {
        if shown {
            items.push(inflexible(h_gap::<S>(spacing::MD)));
            items.push(inflexible(badge(chip, variant)));
        }
    }
    items
}

/// The Tablet variant of piece 8: the same header over card rows (or a muted
/// line when nothing is saved), closed by the same dock. Chips, delete and the
/// fallback tap wire exactly as the phone row's do, and `live` is `None` for
/// the splash exactly as there.
pub fn tablet_saved_list<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    servers: &[SavedServer],
    live: Option<&Live>,
) -> impl View<S> + use<S> {
    let now = epoch_millis();
    // erasure: keep accumulator into Vec<AnyView>
    let mut children = vec![
        any(saved_header::<S>(theme, colors)),
        any(v_gap::<S>(spacing::MD)),
    ];
    if servers.is_empty() {
        children.push(any(
            text(NO_SAVED_SERVERS).style(inked(&theme.type_scale.body_small, colors.fg_muted))
        ));
    }
    for (index, server) in servers.iter().enumerate() {
        if index > 0 {
            children.push(any(v_gap::<S>(spacing::SM)));
        }
        children.push(any(tablet_saved_row(
            theme, colors, server, servers, index, now, live,
        )));
    }
    if !servers.is_empty() {
        children.push(any(dock(theme, colors, servers, live)));
    }
    block_column::<S, _>(children)
}

/// A bordered card row: the label's initial, the label over
/// `host:port · trust · recency`, the chips, and the delete target.
fn tablet_saved_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    server: &SavedServer,
    servers: &[SavedServer],
    index: usize,
    now_ms: i64,
    live: Option<&Live>,
) -> impl View<S> + use<S> {
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
        inflexible(avatar(initial).size(AVATAR).decorative()),
        inflexible(h_gap::<S>(spacing::MD)),
        flexible(
            1,
            block_column::<S, _>((
                text(server.label.clone()).style(inked(&theme.type_scale.body_small, colors.fg)),
                v_gap::<S>(spacing::MICRO),
                text(meta).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
            )),
        ),
    ];
    items.extend(chips::<S>(server, locked(server, servers, live)));
    let content = centered_row::<S>(vec![
        flexible(1, tappable(centered_row::<S>(items), live, index)),
        inflexible(h_gap::<S>(spacing::MD)),
        inflexible(delete_affordance(colors, server, index, live)),
    ]);
    let inset = EdgeInsets {
        left: spacing::BASE,
        top: spacing::XS,
        right: spacing::TIGHT,
        bottom: spacing::XS,
    };
    container(Padding(inset, content))
        .fill(colors.surface)
        .radius(theme.shape.medium)
        .border(colors.outline, HAIRLINE)
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
) -> impl View<S> + use<S> {
    let Some(live) = live else {
        return Either::Left(
            SizedBox::<S>(Some(DELETE_TARGET), Some(DELETE_TARGET)).child(Align(
                Alignment::CENTER,
                icon(icons::DELETE).size(DELETE_MARK).color(colors.fg_dim),
            )),
        );
    };
    let on_delete = Rc::clone(&live.on_saved_delete);
    // A labelled `Role::Button`, which a bare `GestureDetector` could not give a screen reader.
    Either::Right(
        icon_button(
            icons::DELETE,
            delete_label(&server.label),
            move |_state: &mut S| on_delete(index),
        )
        .ink(colors.fg_dim)
        .icon_size(DELETE_MARK)
        .box_size(Size::new(DELETE_TARGET, DELETE_TARGET))
        .radius(DELETE_TARGET / 2.0),
    )
}

/// The delete affordance's accessible name — a bare "Delete" would announce
/// a list of identical controls.
fn delete_label(label: &str) -> String {
    format!("Delete {label}")
}

/// The saved-server rows the splash's copy of piece 8 renders.
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
