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
//! selected on the next. Choosing the implementation is this file's whole job,
//! invisible to the screen, the controller and the use cases (which only ever
//! name the trait): production builds [`GrpcTerminalRepository`], the real
//! `muxrd`-backed implementation, over the app's one [`ConnectionController`]
//! (`crate::MuxrApp::init` publishes it *before* this feature's own `provide`
//! runs, so `expect_connection_controller` is always safe here).
//!
//! # The headless-test seam
//!
//! [`provide`] runs from the *exact same* `MuxrApp::init` call whether it's a
//! real app launch or a `tests/*.rs` integration suite mounting the app root —
//! there is no `cfg(test)` to branch on here, since an integration test binary
//! links the *normal* build of this crate, never the one compiled with
//! `--cfg test`. So a suite opts out **before** `provide` runs, via
//! [`use_mock_terminal_for_tests`]: the one call site is
//! `tests/support::setup()`, which every suite already calls ahead of mounting
//! anything, so no individual `tests/*.rs` file needs to know this seam exists.
//! Without it, every headless suite would route the screen's spaces/tabs
//! through a real `muxrd` connection that `connection::use_mock_connection_for_tests`
//! has already made mock-only, and the seeded `docs/terminal.html` facts each
//! suite asserts on would have nowhere to come from. The exact same seam shape
//! as `crate::features::sessions::di::use_mock_sessions_for_tests`.
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
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::terminal::data::repositories::{
    GrpcTerminalRepository, MockTerminalRepository,
};
use crate::features::terminal::domain::repositories::TerminalRepository;

/// The shared repository handle this feature publishes and resolves — a type
/// alias so the provide/expect pair can never drift apart (context lookup is
/// by exact type).
pub type SharedTerminalRepository = Arc<dyn TerminalRepository + Send + Sync>;

/// Set by [`use_mock_terminal_for_tests`], read by
/// [`platform_terminal_repository`]: `false` (the real `muxrd`-backed
/// repository) for every real app run, flipped to `true` once per test process
/// before [`provide`] ever runs — see the [module docs](self)' headless-test
/// seam section.
static USE_MOCK_TERMINAL_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real, `muxrd`-backed repository for the
/// rest of its life: every later [`provide`] call constructs
/// [`MockTerminalRepository`] instead of [`GrpcTerminalRepository`].
/// Idempotent, and a no-op once already set. Must run **before** `provide`
/// (i.e. before `MuxrApp::init`) to have any effect — `tests/support::setup`
/// is the one call site outside this module; see the [module docs](self).
pub fn use_mock_terminal_for_tests() {
    USE_MOCK_TERMINAL_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// Wire this feature's own dependencies into context: the one
/// [`TerminalRepository`] implementation, constructed once and published under
/// the root owner. See the [module docs](self).
pub fn provide() {
    provide_context(platform_terminal_repository());
}

/// The [`TerminalRepository`] [`provide`] publishes: the real, `muxrd`-backed
/// [`GrpcTerminalRepository`], unless [`use_mock_terminal_for_tests`] opted
/// this process out (every headless suite).
fn platform_terminal_repository() -> SharedTerminalRepository {
    if USE_MOCK_TERMINAL_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockTerminalRepository::seeded());
    }
    Arc::new(GrpcTerminalRepository::new(expect_connection_controller()))
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
