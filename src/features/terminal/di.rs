//! `terminal`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//!
//! It constructs this feature's one app-scoped singleton — the
//! [`TerminalRepository`] implementation — and publishes it under the **root
//! owner** via [`frust::provide_context`], so every mount of the `/terminal`
//! screen resolves the *same* store and a space selected on one visit is still
//! selected on the next. Choosing the implementation is this file's whole job:
//! swapping the mock for the real `muxrd` client is a one-line edit here,
//! invisible to the screen, the controller and the use cases (which only ever
//! name the trait).
//!
//! # The other repository this screen needs is not published here
//!
//! Rename reads and writes a **session's** name, which the `sessions` feature
//! owns; the terminal controller resolves that repository through
//! `sessions`' own
//! [`expect_sessions_repository`](crate::features::sessions::di::expect_sessions_repository)
//! rather than this feature publishing a second handle to the same store. One
//! store, one publisher — which is exactly what makes a rename performed from
//! the terminal visible in the session list on the way back.
//!
//! # Why `Arc<dyn …>` and not a generic
//!
//! `use_controller`'s bounds require the hosted controller to be
//! `Send + Sync + 'static`, and frust's context requires the same of anything
//! provided — so the repository travels as
//! `Arc<dyn TerminalRepository + Send + Sync>` (huddle's DI convention), which
//! also keeps `TerminalController` free of a repository type parameter.

use std::sync::Arc;

use frust::{provide_context, use_context};

use crate::features::terminal::data::repositories::MockTerminalRepository;
use crate::features::terminal::domain::repositories::TerminalRepository;

/// The shared repository handle this feature publishes and resolves — a type
/// alias so the provide/expect pair can never drift apart (context lookup is
/// by exact type).
pub type SharedTerminalRepository = Arc<dyn TerminalRepository + Send + Sync>;

/// Wire this feature's own dependencies into context: the one
/// [`TerminalRepository`] implementation, constructed once and published under
/// the root owner. See the [module docs](self).
pub fn provide() {
    let repository: SharedTerminalRepository = Arc::new(MockTerminalRepository::seeded());
    provide_context(repository);
}

/// Resolve the repository [`provide`] published.
///
/// # Panics
///
/// Panics if called before `MuxrApp::init` has run — the same contract
/// `crate::connection::expect_connection_controller` documents. Every call
/// site (the terminal screen's `use_controller` factory) runs after it, so
/// this is never hit in practice; a headless test that mounts the screen
/// without the composition root gets a loud, immediate failure rather than a
/// silently empty terminal.
pub fn expect_terminal_repository() -> SharedTerminalRepository {
    use_context::<SharedTerminalRepository>().expect(
        "expect_terminal_repository: features::terminal::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
