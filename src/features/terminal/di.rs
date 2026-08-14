//! `terminal`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`. It constructs
//! this feature's one app-scoped singleton — the [`TerminalRepository`]
//! implementation — and publishes it under the **root owner**, so every mount of
//! `/terminal` resolves the *same* store and a space selected on one visit is still
//! selected on the next. Choosing the implementation is this file's whole job.
//!
//! **The headless-test seam.** `provide` runs from the same `MuxrApp::init` call
//! whether this is a real launch or a `tests/*.rs` suite, and an integration binary
//! links the *normal* build of the crate, so there is no `cfg(test)` to branch on. A
//! suite opts out **before** `provide` runs, via [`use_mock_terminal_for_tests`],
//! whose one call site is `tests/support::setup()`.
//!
//! Rename is **not** published here: a session's name is the `sessions` feature's,
//! and the controller resolves that repository through `sessions`' own `di` — which
//! is what makes a rename from the terminal visible in the session list.
//!
//! The repository travels as `Arc<dyn TerminalRepository + Send + Sync>` because
//! `use_controller` and frust's context both require `Send + Sync + 'static`.

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
/// [`platform_terminal_repository`]: `false` for every real app run, flipped to
/// `true` once per test process before [`provide`] ever runs.
static USE_MOCK_TERMINAL_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real, `muxrd`-backed repository for the rest
/// of its life: every later [`provide`] constructs [`MockTerminalRepository`]
/// instead. Idempotent, and must run **before** `provide` to have any effect.
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
/// Panics if called before `MuxrApp::init` has run, so a headless test that mounts
/// the screen without the composition root fails loudly.
pub fn expect_terminal_repository() -> SharedTerminalRepository {
    use_context::<SharedTerminalRepository>().expect(
        "expect_terminal_repository: features::terminal::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
