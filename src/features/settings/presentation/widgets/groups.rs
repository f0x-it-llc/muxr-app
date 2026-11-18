use std::rc::Rc;

use frust::{AnyView, ButtonStyle, any, button};

use crate::connection::{ServerBackend, TrustMode};
use crate::features::settings::domain::entities::{AppearancePrefs, PollPrefs};
use crate::features::settings::presentation::app_pages::AppSettingsState;
use crate::features::settings::presentation::controller::TokenListView;
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::Palette;

use super::misc::bell_notifications_row;
use super::pickers::PickerKind;
use super::rows::{self, RowAction, ValueTone, settings_row};
use super::server_sheet::{DELETE_LABEL, FORGET_SERVER_LABEL, SAVED_CREDENTIAL_LABEL};

const APP_SETTINGS_VALUE: &str = "fonts, bell, Muxr Pro, servers";
const GETTING_STARTED_VALUE: &str = "pair a server in three steps";

/// The same copy the saved-server sheet prints for these two actions.
const SAVED_CREDENTIAL_VALUE: &str = "on this device";
const FORGET_SERVER_VALUE: &str = "remove the profile and its credential";
const TOKENS_LABEL: &str = "auth tokens";
const TOKENS_LOADING_VALUE: &str = "loading\u{2026}";
const TOKENS_FAILED_VALUE: &str = "couldn't load tokens";

/// `/settings` is not route-guarded, so a deep link or a disconnect racing a rebuild shows this.
pub(super) const DISCONNECTED_VALUE: &str = "not connected";

/// `<n> tokens · <m> read-only`, singular for one; the load states stand in for it until a list
/// has landed.
fn token_count_value(total: usize, read_only: usize, loaded: bool, failed: bool) -> String {
    if failed {
        return TOKENS_FAILED_VALUE.to_string();
    }
    if !loaded {
        return TOKENS_LOADING_VALUE.to_string();
    }
    let noun = if total == 1 { "token" } else { "tokens" };
    format!("{total} {noun} \u{b7} {read_only} read-only")
}

/// Pushes the tokens page.
pub(in super::super) fn tokens_row(
    view: &TokenListView,
    loaded: bool,
    failed: bool,
    colors: &Palette,
) -> AnyView<SettingsState> {
    let [_, _, read_only] = view.counts;
    settings_row(
        rows::MARK_TOKENS,
        TOKENS_LABEL,
        token_count_value(view.total, read_only, loaded, failed),
        ValueTone::Plain,
        RowAction::AuthTokens,
        colors,
    )
}

/// Shown only for a server holding a saved credential; the delete runs the connected server's
/// own confirm and flow.
pub(in super::super) fn saved_credential_row(colors: &Palette) -> AnyView<SettingsState> {
    let delete = button(DELETE_LABEL, |state: &mut SettingsState| {
        pages::open_delete_credential_dialog(state)
    })
    .style(ButtonStyle::Danger)
    .small();
    rows::action_row(
        rows::MARK_CREDENTIAL,
        SAVED_CREDENTIAL_LABEL,
        SAVED_CREDENTIAL_VALUE,
        false,
        any(delete),
        None,
        colors,
    )
}

pub(in super::super) fn forget_server_row(colors: &Palette) -> AnyView<SettingsState> {
    rows::action_row(
        rows::MARK_FORGET,
        FORGET_SERVER_LABEL,
        FORGET_SERVER_VALUE,
        true,
        rows::chevron(colors),
        Some(Rc::new(|state: &mut SettingsState| {
            pages::open_forget_server_dialog(state)
        })),
        colors,
    )
}

pub(super) fn backend_stat_label(backend: ServerBackend) -> &'static str {
    match backend {
        ServerBackend::Zellij => "zellij",
        ServerBackend::Herdr => "herdr",
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

pub(in super::super) fn app_settings_rows(colors: &Palette) -> Vec<AnyView<SettingsState>> {
    vec![settings_row(
        rows::MARK_APP_SETTINGS,
        "app settings",
        APP_SETTINGS_VALUE,
        ValueTone::Plain,
        RowAction::AppSettings,
        colors,
    )]
}

pub(in super::super) fn about_rows(colors: &Palette) -> Vec<AnyView<AppSettingsState>> {
    vec![
        settings_row(
            rows::MARK_VERSION,
            "version",
            env!("CARGO_PKG_VERSION"),
            ValueTone::Plain,
            RowAction::Static,
            colors,
        ),
        settings_row(
            rows::MARK_GETTING_STARTED,
            "getting started",
            GETTING_STARTED_VALUE,
            ValueTone::Plain,
            RowAction::GettingStarted,
            colors,
        ),
    ]
}

pub(in super::super) fn appearance_rows(
    prefs: &AppearancePrefs,
    colors: &Palette,
) -> Vec<AnyView<AppSettingsState>> {
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
) -> Vec<AnyView<AppSettingsState>> {
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
            |state: &mut AppSettingsState| state.actions.toggle_bell(),
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
    fn the_token_count_names_both_totals_and_is_singular_for_one() {
        assert_eq!(
            token_count_value(1, 0, true, false),
            "1 token \u{b7} 0 read-only"
        );
        assert_eq!(
            token_count_value(3, 2, true, false),
            "3 tokens \u{b7} 2 read-only"
        );
        assert_eq!(
            token_count_value(0, 0, true, false),
            "0 tokens \u{b7} 0 read-only"
        );
    }

    #[test]
    fn the_token_count_shows_the_load_state_until_a_list_lands() {
        assert_eq!(token_count_value(0, 0, false, false), TOKENS_LOADING_VALUE);
        assert_eq!(token_count_value(0, 0, true, true), TOKENS_FAILED_VALUE);
    }

    #[test]
    fn the_version_row_prints_the_number_without_the_product_name() {
        assert_eq!(version_number("zellij 0.44.3"), "0.44.3");
        assert_eq!(version_number("herdr 1.2"), "1.2");
        assert_eq!(version_number("0.44.3"), "0.44.3");
    }
}
