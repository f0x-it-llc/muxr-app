//! `sessions`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//!
//! It constructs this feature's one app-scoped singleton — the
//! [`SessionsRepository`] implementation — and publishes it under the **root
//! owner** via [`frust::provide_context`], so every mount of the `/sessions`
//! screen resolves the *same* store through [`expect_sessions_repository`] and
//! a session created on one visit is still there on the next. Choosing the
//! implementation is this file's whole job: swapping the mock for the real
//! `muxrd` client is a one-line edit here, invisible to the screen, the
//! controller and the use cases (which only ever name the trait).
//!
//! # Why `Arc<dyn …>` and not a generic
//!
//! `use_controller`'s bounds require the hosted controller to be
//! `Send + Sync + 'static`, and frust's context requires the same of anything
//! provided — so the repository travels as
//! `Arc<dyn SessionsRepository + Send + Sync>` (huddle's DI convention), which
//! also keeps `SessionsController` free of a repository type parameter.

use std::sync::Arc;

use frust::{provide_context, use_context};

use crate::features::sessions::data::repositories::MockSessionsRepository;
use crate::features::sessions::domain::repositories::SessionsRepository;

/// The shared repository handle this feature publishes and resolves — a type
/// alias so the provide/expect pair can never drift apart (context lookup is
/// by exact type).
pub type SharedSessionsRepository = Arc<dyn SessionsRepository + Send + Sync>;

/// Wire this feature's own dependencies into context: the one
/// [`SessionsRepository`] implementation, constructed once and published under
/// the root owner. See the [module docs](self).
pub fn provide() {
    let repository: SharedSessionsRepository = Arc::new(MockSessionsRepository::seeded());
    provide_context(repository);
}

/// Resolve the repository [`provide`] published.
///
/// # Panics
///
/// Panics if called before `MuxrApp::init` has run — the same contract
/// `crate::connection::expect_connection_controller` documents. Every call
/// site (the sessions screen's `use_controller` factory) runs after it, so
/// this is never hit in practice; a headless test that mounts the screen
/// without the composition root gets a loud, immediate failure rather than a
/// silently empty list.
pub fn expect_sessions_repository() -> SharedSessionsRepository {
    use_context::<SharedSessionsRepository>().expect(
        "expect_sessions_repository: features::sessions::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
