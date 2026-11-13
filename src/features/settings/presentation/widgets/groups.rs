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

/// `/settings` is not route-guarded, so a deep link or a disconnect racing a rebuild shows this.
const DISCONNECTED_VALUE: &str = "not connected";

/// A token whose refresh already removed it keeps its scramble row at its old index (clamped),
/// orphans in ascending index, so nothing under it jumps while the collapse plays out.
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

fn backend_mark(backend: ServerBackend) -> RowMark {
    match backend {
        ServerBackend::Zellij => rows::MARK_ZELLIJ,
        ServerBackend::Herdr => rows::MARK_HERDR,
    }
}

fn backend_version_label(backend: ServerBackend) -> &'static str {
    match backend {
        ServerBackend::Zellij => "zellij version",
        ServerBackend::Herdr => "herdr version",
    }
}

/// Branches on the [`TrustMode`] variant, never on `ServerInfo::tls_label` (display text).
pub(super) fn tls_posture(mode: TrustMode) -> (&'static str, ValueTone) {
    match mode {
        TrustMode::NoVerification => ("insecure (plaintext)", ValueTone::Warn),
        TrustMode::System => ("public CA", ValueTone::Ok),
        TrustMode::Pinned => ("pinned certificate", ValueTone::Ok),
    }
}

pub(super) fn version_number(version: &str) -> &str {
    version.rsplit_once(' ').map_or(version, |(_, tail)| tail)
}

pub(in super::super) fn appearance_rows(
    prefs: &AppearancePrefs,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    vec![
        settings_row(
            rows::MARK_FONT_FAMILY,
            "font family",
            // The display name, not the persisted family.
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
        assert_eq!(version_number("zellij 0.44.3"), "0.44.3");
        assert_eq!(version_number("herdr 1.2"), "1.2");
        assert_eq!(version_number("0.44.3"), "0.44.3");
    }
}
