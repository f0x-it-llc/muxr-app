//! `settings` use cases — one file per `<verb><noun>` action, one per
//! repository method: [`ListTokens`]/[`CreateToken`]/[`RevokeToken`] over
//! `AuthTokensRepository`, [`LoadPreferences`]/[`UpdateAppearance`]/
//! [`UpdatePollPrefs`] over `PreferencesRepository`. Each is a thin,
//! injected-repository [`UseCase`](clean_signals::UseCase) with no logic
//! beyond delegation today — the value of the seam is that
//! [`SettingsController`](crate::features::settings::presentation::controller::SettingsController)
//! drives every call through `ControllerCore::run` (activity tracking, retry
//! policy, one failure-emission site) rather than touching a repository
//! directly; a future business rule lands in the matching file here without
//! any call site moving. Never call `execute` directly from a controller —
//! always through `ControllerCore::run`/`run_into`.

pub mod create_token;
pub mod list_tokens;
pub mod load_preferences;
pub mod revoke_token;
pub mod update_appearance;
pub mod update_poll_prefs;

pub use create_token::CreateToken;
pub use list_tokens::ListTokens;
pub use load_preferences::LoadPreferences;
pub use revoke_token::RevokeToken;
pub use update_appearance::UpdateAppearance;
pub use update_poll_prefs::UpdatePollPrefs;
