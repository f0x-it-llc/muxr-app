//! Repository contracts for `data` to implement: server-side tokens and device-local preferences are separate traits.

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AppearancePrefs, AuthToken, CreateTokenRequest, MintedToken, PollPrefs, Preferences,
};

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait AuthTokensRepository {
    /// In the daemon's order; never carries a secret.
    async fn list_tokens(&self) -> Result<Vec<AuthToken>, AppFailure>;

    /// The one-time secret is returned only here.
    async fn create_token(&self, request: CreateTokenRequest) -> Result<MintedToken, AppFailure>;

    async fn revoke_token(&self, name: String) -> Result<(), AppFailure>;
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait PreferencesRepository {
    async fn load_preferences(&self) -> Result<Preferences, AppFailure>;

    async fn save_appearance(&self, prefs: AppearancePrefs) -> Result<AppearancePrefs, AppFailure>;

    async fn save_poll_prefs(&self, prefs: PollPrefs) -> Result<PollPrefs, AppFailure>;
}
