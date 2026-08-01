//! `settings`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//!
//! It constructs this feature's two app-scoped singletons — the
//! [`AuthTokensRepository`] and [`PreferencesRepository`] implementations — and
//! publishes each under the **root owner** via [`frust::provide_context`], so
//! every mount of the `/settings` screen resolves the *same* stores through
//! [`expect_auth_tokens_repository`]/[`expect_preferences_repository`]: a token
//! revoked on one visit is still gone on the next, and a font size picked on
//! one is still picked. Choosing the implementations is this file's whole job
//! — swapping the mocks for the real `muxrd` client and a storage plugin is a
//! two-line edit here, invisible to the screen, the controller and the use
//! cases (which only ever name the traits).
//!
//! # Why `Arc<dyn …>` and not a generic
//!
//! `use_controller`'s bounds require the hosted controller to be
//! `Send + Sync + 'static`, and frust's context requires the same of anything
//! provided — so each repository travels as `Arc<dyn … + Send + Sync>`
//! (huddle's DI convention), which also keeps
//! [`SettingsController`](crate::features::settings::presentation::controller::SettingsController)
//! free of two repository type parameters.

use std::sync::Arc;

use frust::{provide_context, use_context};

use crate::features::settings::data::repositories::{
    MockAuthTokensRepository, MockPreferencesRepository,
};
use crate::features::settings::domain::repositories::{
    AuthTokensRepository, PreferencesRepository,
};

/// The shared token-repository handle this feature publishes and resolves — a
/// type alias so the provide/expect pair can never drift apart (context lookup
/// is by exact type).
pub type SharedAuthTokensRepository = Arc<dyn AuthTokensRepository + Send + Sync>;

/// The shared preferences-repository handle, for the same reason.
pub type SharedPreferencesRepository = Arc<dyn PreferencesRepository + Send + Sync>;

/// Wire this feature's own dependencies into context: the two repository
/// implementations, each constructed once and published under the root owner.
/// See the [module docs](self).
pub fn provide() {
    let tokens: SharedAuthTokensRepository = Arc::new(MockAuthTokensRepository::seeded());
    provide_context(tokens);
    let preferences: SharedPreferencesRepository = Arc::new(MockPreferencesRepository::seeded());
    provide_context(preferences);
}

/// Resolve the token repository [`provide`] published.
///
/// # Panics
///
/// Panics if called before `MuxrApp::init` has run — the same contract
/// `crate::connection::expect_connection_controller` documents. Every call
/// site (the settings screen's `use_controller` factory) runs after it, so
/// this is never hit in practice; a headless test that mounts the screen
/// without the composition root gets a loud, immediate failure rather than a
/// silently empty token list.
pub fn expect_auth_tokens_repository() -> SharedAuthTokensRepository {
    use_context::<SharedAuthTokensRepository>().expect(
        "expect_auth_tokens_repository: features::settings::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// Resolve the preferences repository [`provide`] published. Same panic
/// contract as [`expect_auth_tokens_repository`].
pub fn expect_preferences_repository() -> SharedPreferencesRepository {
    use_context::<SharedPreferencesRepository>().expect(
        "expect_preferences_repository: features::settings::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
