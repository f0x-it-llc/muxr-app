//! The `connection`, `auth tokens`, `terminal appearance` and `session poll`
//! groups' row lists — pure arrangement over [`super::rows`]/[`super::tokens`]
//! (each read off the connection controller or the settings controller by
//! `pages::SettingsScreen::build`, never resolving anything itself).

use frust::AnyView;

use crate::connection::{ServerBackend, ServerInfo, TrustMode};
use crate::features::settings::domain::entities::{AppearancePrefs, AuthToken, PollPrefs};
use crate::features::settings::presentation::pages::SettingsState;
use crate::features::settings::presentation::scramble::{RevokeScramble, ScramblePose};
use crate::ui::palette::Palette;

use super::misc::bell_notifications_row;
use super::pickers::PickerKind;
use super::rows::{self, RowAction, RowMark, ValueTone, settings_row};
use super::tokens::{scrambling_token_item, token_item};

/// The `connection` group's value while nothing is connected. `/settings` is
/// deliberately not route-guarded, so this is reachable — a deep link, or a
/// disconnect racing a rebuild — and a blank row would be the worse failure.
const DISCONNECTED_VALUE: &str = "not connected";

/// The `auth tokens` group's rows.
///
/// One `token_item` per token with no revoke in flight; this frame's scramble
/// view for every token that has one; and — for a revoke whose own refresh
/// already took its token out of `tokens` — the same scramble view
/// re-inserted at the position that row is vanishing from (clamped, since the
/// list it was snapshotted against may have shrunk), so nothing under it
/// jumps while the collapse plays out. Orphans go back in ascending order, so
/// two at once keep their original order between them.
pub(in super::super) fn token_rows(
    tokens: &[AuthToken],
    showing: &[(RevokeScramble, ScramblePose)],
    read_only: bool,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    let animating = |token: &AuthToken| {
        showing
            .iter()
            .find(|(entry, _)| entry.token.name == token.name)
    };
    let mut rows: Vec<AnyView<SettingsState>> = tokens
        .iter()
        .map(|token| match animating(token) {
            Some((entry, pose)) => scrambling_token_item(&entry.token, pose, colors),
            None => token_item(token, colors, !read_only),
        })
        .collect();

    let mut orphans: Vec<&(RevokeScramble, ScramblePose)> = showing
        .iter()
        .filter(|(entry, _)| !tokens.iter().any(|token| token.name == entry.token.name))
        .collect();
    orphans.sort_by_key(|(entry, _)| entry.index);
    for (entry, pose) in orphans {
        let at = entry.index.min(rows.len());
        rows.insert(at, scrambling_token_item(&entry.token, pose, colors));
    }
    rows
}

/// The `connection` group: the endpoint, the TLS posture, the latency, and one version row
/// per backend the server reports. Every value is read off `info`; `None`
/// prints [`DISCONNECTED_VALUE`] rather than nothing.
pub(in super::super) fn connection_rows(
    info: Option<&ServerInfo>,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    let (endpoint, endpoint_tone) = match info {
        Some(info) => (info.endpoint.clone(), ValueTone::Ok),
        None => (DISCONNECTED_VALUE.to_string(), ValueTone::Plain),
    };
    let (tls_value, tls_tone) = match info {
        Some(info) => tls_posture(info.trust_mode),
        None => (DISCONNECTED_VALUE, ValueTone::Plain),
    };
    let latency = info.map_or_else(
        || DISCONNECTED_VALUE.to_string(),
        |info| format!("{}ms", info.latency_ms),
    );
    let mut rows = vec![
        settings_row(
            rows::MARK_SERVER_URL,
            "server url",
            endpoint,
            endpoint_tone,
            RowAction::Static,
            colors,
        ),
        settings_row(
            rows::MARK_TLS,
            "tls certificate",
            tls_value,
            tls_tone,
            RowAction::Static,
            colors,
        ),
        settings_row(
            rows::MARK_LATENCY,
            "latency",
            latency,
            ValueTone::Plain,
            RowAction::Static,
            colors,
        ),
    ];
    rows.extend(backend_version_rows(info, colors));
    rows
}

/// One row per backend `info` actually reported, or the legacy
/// single-version fallback for an older server that reported none — always
/// at least one row, so this group never goes silent about the multiplexer
/// it's attached to (or, while disconnected, the one it *was*).
fn backend_version_rows(
    info: Option<&ServerInfo>,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    match info {
        Some(info) if !info.backend_versions.is_empty() => info
            .backend_versions
            .iter()
            .map(|backend_version| {
                settings_row(
                    backend_mark(backend_version.backend),
                    backend_version_label(backend_version.backend),
                    backend_version.version.clone(),
                    ValueTone::Plain,
                    RowAction::Static,
                    colors,
                )
            })
            .collect(),
        Some(info) => vec![settings_row(
            rows::MARK_ZELLIJ,
            "zellij version",
            version_number(&info.version).to_string(),
            ValueTone::Plain,
            RowAction::Static,
            colors,
        )],
        None => vec![settings_row(
            rows::MARK_ZELLIJ,
            "zellij version",
            DISCONNECTED_VALUE,
            ValueTone::Plain,
            RowAction::Static,
            colors,
        )],
    }
}

/// The `.set-ic` mark for `backend`'s version row — the backend's own
/// initial.
fn backend_mark(backend: ServerBackend) -> RowMark {
    match backend {
        ServerBackend::Zellij => rows::MARK_ZELLIJ,
        ServerBackend::Herdr => rows::MARK_HERDR,
    }
}

/// …and its row label.
fn backend_version_label(backend: ServerBackend) -> &'static str {
    match backend {
        ServerBackend::Zellij => "zellij version",
        ServerBackend::Herdr => "herdr version",
    }
}

/// What the `tls certificate` row prints for a posture, and in which hue.
///
/// Branches on the [`TrustMode`] **variant**, never on `ServerInfo::tls_label`
/// (that field is display text, per `crate::connection`'s own rule). The
/// words are the redesign's rather than that label's — its "No verification"
/// names the *posture*, where screen 5 names the *consequence*.
pub(super) fn tls_posture(mode: TrustMode) -> (&'static str, ValueTone) {
    match mode {
        TrustMode::NoVerification => ("insecure (plaintext)", ValueTone::Warn),
        TrustMode::System => ("public CA", ValueTone::Ok),
        TrustMode::Pinned => ("pinned certificate", ValueTone::Ok),
    }
}

/// The version number out of a `"<product> <version>"` string —
/// `"zellij 0.44.3"` → `"0.44.3"`, which is what the row prints (its *label*
/// already says `zellij version`). A string with no space is its own
/// version.
pub(super) fn version_number(version: &str) -> &str {
    version.rsplit_once(' ').map_or(version, |(_, tail)| tail)
}

/// The `terminal appearance` group: three picker rows.
pub(in super::super) fn appearance_rows(
    prefs: &AppearancePrefs,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    vec![
        settings_row(
            rows::MARK_FONT_FAMILY,
            "font family",
            // The *display* name, not the persisted family: a downloaded
            // Nerd Font stores as `BlexMono Nerd Font Mono` and is called
            // `IBM Plex Mono Nerd Font Mono` everywhere a person reads it.
            crate::fonts::display_name(&prefs.font_family),
            ValueTone::Plain,
            RowAction::Picker(PickerKind::FontFamily),
            colors,
        ),
        settings_row(
            rows::MARK_FONT_SIZE,
            "font size",
            prefs.font_size_label(),
            ValueTone::Plain,
            RowAction::Picker(PickerKind::FontSize),
            colors,
        ),
        settings_row(
            rows::MARK_CURSOR,
            "cursor style",
            prefs.cursor_style.label(),
            ValueTone::Plain,
            RowAction::Picker(PickerKind::CursorStyle),
            colors,
        ),
    ]
}

/// The `session poll` group: one picker row and the bell switch.
pub(in super::super) fn poll_rows(
    prefs: &PollPrefs,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    vec![
        settings_row(
            rows::MARK_REFRESH,
            "layout refresh",
            prefs.layout_refresh_label(),
            ValueTone::Plain,
            RowAction::Picker(PickerKind::LayoutRefresh),
            colors,
        ),
        bell_notifications_row(
            prefs.bell_notifications,
            colors,
            |state: &mut SettingsState| state.actions.toggle_bell(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_validation_skipping_posture_warns() {
        // The redesign's own correction, as a tripwire: `insecure
        // (plaintext)` must never read as a success state, and a validated
        // posture must never read as a warning.
        assert!(matches!(
            tls_posture(TrustMode::NoVerification),
            ("insecure (plaintext)", ValueTone::Warn)
        ));
        assert!(matches!(
            tls_posture(TrustMode::System),
            ("public CA", ValueTone::Ok)
        ));
        assert!(matches!(
            tls_posture(TrustMode::Pinned),
            ("pinned certificate", ValueTone::Ok)
        ));
    }

    #[test]
    fn the_version_row_prints_the_number_without_the_product_name() {
        // The row's *label* already says `zellij version`; `ServerInfo`
        // carries the joined `"zellij 0.44.3"` because the sessions strip
        // prints it that way.
        assert_eq!(version_number("zellij 0.44.3"), "0.44.3");
        assert_eq!(version_number("herdr 1.2"), "1.2");
        assert_eq!(version_number("0.44.3"), "0.44.3");
    }
}
