//! `settings` domain repository contracts, implemented by `data`.
//!
//! Two traits, not one: [`AuthTokensRepository`] is *server* state over
//! `muxrd` (list, mint, revoke); [`PreferencesRepository`] is *device-local*
//! state over a storage plugin. Splitting them lets one be a gRPC client and
//! the other a plugin without either growing methods it has no business
//! owning.
//!
//! Both are `async` (dual `cfg_attr`, since `wasm32`'s single-threaded event
//! loop can't require `Send` futures) and consumed as `Arc<dyn … + Send +
//! Sync>`, so a real implementation and an in-memory test mock sit behind the
//! same seam additively — `di.rs` chooses per repository which one `provide`
//! constructs. Every method fails with the app-wide [`AppFailure`]
//! vocabulary; `data` owns the one mapping site into it.

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AppearancePrefs, AuthToken, CreateTokenRequest, MintedToken, PollPrefs, Preferences,
};

/// Read/write access to the connected server's auth tokens.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait AuthTokensRepository {
    /// Every token the server currently holds, in display order (the daemon's
    /// own ordering — this layer never re-sorts). Never carries a secret —
    /// see [`MintedToken`]'s own doc.
    async fn list_tokens(&self) -> Result<Vec<AuthToken>, AppFailure>;

    /// Mint a token per `request` (name + whether it should be read-only),
    /// returning it as stored **plus** the one-time secret the daemon hands
    /// back only here.
    async fn create_token(&self, request: CreateTokenRequest) -> Result<MintedToken, AppFailure>;

    /// Revoke the token called `name` — see
    /// [`AuthToken::name`](super::entities::AuthToken::name) for why a name is
    /// the key here where a session needs an id.
    async fn revoke_token(&self, name: String) -> Result<(), AppFailure>;
}

/// Read/write access to this device's terminal + poll preferences.
///
/// The two savers are separate rather than one `save(Preferences)` because the
/// screen never edits both halves at once: a picker writes one appearance
/// field, the bell switch writes one poll field, and a single combined write
/// would make each of those a read-modify-write of state it isn't touching.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait PreferencesRepository {
    /// Both preference groups, in one read (see [`Preferences`]).
    async fn load_preferences(&self) -> Result<Preferences, AppFailure>;

    /// Persist the `terminal appearance` group, returning it as stored.
    async fn save_appearance(&self, prefs: AppearancePrefs) -> Result<AppearancePrefs, AppFailure>;

    /// Persist the `session poll` group, returning it as stored.
    async fn save_poll_prefs(&self, prefs: PollPrefs) -> Result<PollPrefs, AppFailure>;
}
