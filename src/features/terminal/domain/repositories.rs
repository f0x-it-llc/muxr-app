//! `terminal` domain repository contract — the seam this feature's use cases
//! depend on and `data` implements (framework-free, per
//! `crate::features::terminal::domain`'s module doc).
//!
//! [`TerminalRepository`] is the whole contract: read the attached session's
//! spaces/tabs, read one tab's lines, and detach — exactly what
//! `docs/terminal.html` (v2) affords, and nothing speculative. Writing a
//! session's *name* is deliberately **not** here: a session's name belongs to
//! the `sessions` feature's own repository, and the terminal screen's rename
//! delegates to it (see
//! [`RenameAttachedSession`](super::use_cases::RenameAttachedSession)) rather
//! than minting a second write path onto the same field.
//!
//! # Why it is already async
//!
//! Today's only implementation is in-memory (`data`'s
//! `MockTerminalRepository`), which needs no `await` at all. The trait is
//! `async` regardless so the real `muxrd` client can be dropped in behind it
//! **additively** — the controller and its use cases already drive every call
//! through `ControllerCore::run`'s async path, so the swap changes one `impl`
//! block and nothing above it. The dual `cfg_attr` is the pair every async
//! trait in this codebase uses (`wasm32`'s single-threaded event loop can't
//! require `Send` futures).
//!
//! # Failure mapping
//!
//! Every method fails with the app-wide [`AppFailure`] vocabulary — the data
//! layer owns the single mapping site into it, so nothing above ever sees a
//! transport error type.

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{Space, TabEntry, TermLine};

/// Read access to the attached session's tab/pane world, plus detach.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait TerminalRepository {
    /// Every space the connected server knows about, in display order, with
    /// the selected one flagged.
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure>;

    /// Every tab in the attached session, in display order, with the active
    /// one flagged.
    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure>;

    /// The scrollback the tab keyed `key` currently shows.
    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure>;

    /// Detach this client from the session with `id`, leaving it running.
    async fn detach_session(&self, id: String) -> Result<(), AppFailure>;
}
