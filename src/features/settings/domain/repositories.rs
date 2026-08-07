//! `settings` domain repository contracts — the seam this feature's use cases
//! depend on and `data` implements (framework-free, per
//! `crate::features::settings::domain`'s module doc).
//!
//! **Two** traits, not one, because the two halves of this screen have
//! genuinely different owners: [`AuthTokensRepository`] is *server* state
//! reached over `muxrd` (list, mint, revoke — exactly what
//! `docs/redesign.html`'s `auth tokens` group affords), while
//! [`PreferencesRepository`] is *device-local* state a
//! secure-storage/shared-preferences plugin backs. Splitting them is what
//! lets one be a gRPC client and the other a plugin without either
//! implementation growing methods it has no business owning.
//!
//! # Why they are already async
//!
//! Both traits are `async` so a real, IO-bound implementation and an
//! in-memory mock can sit behind the same seam **additively** — the
//! controller, its use cases and the screen already drive every call through
//! `ControllerCore::run`'s async path, so swapping one out for the other
//! changes one `impl` block and nothing above it. Every headless suite in
//! `tests/` runs the in-memory mocks (`data`'s own, needing no real `await`);
//! a real app run wires
//! [`data::GrpcAuthTokensRepository`](crate::features::settings::data::repositories::GrpcAuthTokensRepository)/
//! [`data::PrefsRepository`](crate::features::settings::data::repositories::PrefsRepository)
//! instead — `di.rs`'s own doc names the seam each swap runs through. The
//! dual `cfg_attr` is the same pair every async trait in this codebase and
//! `examples/huddle` uses (`wasm32`'s single-threaded event loop can't require
//! `Send` futures).
//!
//! Consumed as `Arc<dyn … + Send + Sync>` (huddle's DI convention), so neither
//! the use cases nor the controller carry a repository type parameter.
//!
//! # Failure mapping
//!
//! Every method fails with the app-wide [`AppFailure`] vocabulary — the data
//! layer owns the single mapping site into it, so nothing above ever sees a
//! transport or storage error type.

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
