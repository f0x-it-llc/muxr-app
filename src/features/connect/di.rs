//! `connect`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//!
//! This is the **only** place `connect`'s repositories are constructed. The
//! Connect screen resolves them with `use_context` inside its own
//! `use_controller` factory (`presentation::pages`'s `ConnectScreen::init`), so
//! construction stays single-site even though the screen remounts on every
//! navigation back to `/`.
//!
//! # Why `provide_context`, not `provide_controller`
//!
//! `clean_signals_frust::provide_controller` *is* `provide_context` (see its own
//! docs) — the two differ only in intent. A repository is not a controller: it
//! owns no signals and has no `ControllerCore` to dispose, so it goes in as a
//! plain context value. `crate::connection::ConnectionController`, which does
//! own signals, uses the controller spelling.
//!
//! # Owner scope
//!
//! `MuxrApp::init` runs under the shell's **root** owner (`frust::run_with_setup`
//! wraps it; `frust::Component::init`'s own docs spell out that a root component
//! has no per-component owner of its own), so the value published here is
//! visible to the whole tree for the process's life and is never disposed.

use std::sync::Arc;

use frust::provide_context;

use super::data::repositories::mock_saved_servers::MockSavedServersRepository;
use super::domain::repositories::SavedServersRepository;

/// The context type the Connect screen resolves. Named once here so the
/// `provide` and the `use_context` on the other side cannot drift — an
/// `Arc<MockSavedServersRepository>` on one side and an
/// `Arc<dyn SavedServersRepository + Send + Sync>` on the other are two
/// different context keys and would simply never find each other.
pub type SavedServers = Arc<dyn SavedServersRepository + Send + Sync>;

/// Wire this feature's own dependencies into context — see the
/// [module docs](self).
pub fn provide() {
    let repository: SavedServers = Arc::new(MockSavedServersRepository::seeded());
    provide_context(repository);
}
