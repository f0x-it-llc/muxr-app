//! `settings` use cases — one file per `<verb><noun>` action (see
//! `examples/huddle`'s `domain/use_cases/` convention).
//!
//! Six, one per method across this feature's two repository contracts:
//! [`ListTokens`]/[`CreateToken`]/[`RevokeToken`] over
//! [`AuthTokensRepository`](super::repositories::AuthTokensRepository), and
//! [`LoadPreferences`]/[`UpdateAppearance`]/[`UpdatePollPrefs`] over
//! [`PreferencesRepository`](super::repositories::PreferencesRepository).
//! Each is a thin, injected-repository [`UseCase`](clean_signals::UseCase)
//! with no logic of its own beyond delegation — deliberately: the mock era has
//! no business rule to encode yet, and the value of the seam is that
//! [`SettingsController`](crate::features::settings::presentation::controller::SettingsController)
//! drives every call through `ControllerCore::run` (activity tracking, retry
//! policy, one failure-emission site) instead of touching a repository
//! directly. A real rule (a name-collision check on mint, a "cannot revoke the
//! token you are connected with" guard) lands in the matching file here without
//! any call site moving.
//!
//! Never call [`UseCase::execute`](clean_signals::UseCase::execute) directly
//! from a controller — always drive it through
//! `ControllerCore::run`/`run_into` (huddle's rule, restated).

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
