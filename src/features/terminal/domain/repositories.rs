//! `terminal` domain repository contract — the seam this feature's use cases
//! depend on and `data` implements (framework-free, per
//! `crate::features::terminal::domain`'s module doc).
//!
//! [`TerminalRepository`] is the whole contract: read the attached session's
//! spaces/tabs, read one tab's lines, switch space/tab, and detach — exactly
//! what the screen affords, and nothing speculative. Writing a session's
//! *name* is deliberately **not** here: a session's name belongs to the
//! `sessions` feature's own repository, and the terminal screen's rename
//! delegates to it (see
//! [`RenameAttachedSession`](super::use_cases::RenameAttachedSession)) rather
//! than minting a second write path onto the same field.
//!
//! # Why it is already async
//!
//! The in-memory implementation (`data`'s `MockTerminalRepository`) needs no
//! `await` at all. The trait is `async` regardless so the real `muxrd` client
//! (`data`'s `GrpcTerminalRepository`) drops in behind it **additively** — the
//! controller and its use cases already drive every call through
//! `ControllerCore::run`'s async path, so the swap changes one `impl` block
//! and nothing above it. The dual `cfg_attr` is the pair every async trait in
//! this codebase uses (`wasm32`'s single-threaded event loop can't require
//! `Send` futures).
//!
//! # Which session, and over which connection
//!
//! The four read/detach methods below were minted for a mock world that is
//! deliberately session-agnostic (`super::entities`' own module docs), so none
//! of them takes a session id — and their signatures are load-bearing for
//! `super::use_cases`, which drives them. A live `muxrd` repository needs two
//! facts they cannot carry: which session to address, and the per-connection
//! routing token the attached stream minted. [`TerminalRepository::bind`] is
//! how it gets them — a plain, synchronous setter the screen calls once on
//! mount and again whenever the stream re-mints its id, defaulting to a no-op
//! so an implementation with no wire behind it ignores it entirely.
//!
//! # Failure mapping
//!
//! Every method fails with the app-wide [`AppFailure`] vocabulary — the data
//! layer owns the single mapping site into it, so nothing above ever sees a
//! transport error type.
//!
//! # Session-repository reuse (a documented layering tension)
//!
//! [`SessionsRepository`] is **not** re-implemented or wrapped here: it is
//! [`crate::features::sessions::domain::repositories::SessionsRepository`],
//! the same contract `sessions`' own screen depends on, so this screen's
//! rename writes through the one store a session's name actually lives in
//! rather than a second, driftable path (see
//! [`RenameAttachedSession`](super::use_cases::RenameAttachedSession)). See
//! `crate::features::terminal::domain`'s module docs for the full accounting
//! of this `domain` → sibling `domain` widening (the same shape `connect`'s
//! own `TrustMode` re-export uses); nothing below crosses into `sessions`'
//! `data` or `presentation`, and `presentation::controller` reaches this
//! trait through here rather than naming `sessions::domain` itself.
//!
//! [`PreferencesRepository`] is re-exported the identical way, for the
//! identical reason: it is
//! [`crate::features::settings::domain::repositories::PreferencesRepository`],
//! the `settings` feature's own device-local preferences store, which the
//! live pane's appearance poll reads through a controller-held handle
//! (`presentation::controller::TerminalController::refresh_appearance`)
//! rather than a per-tick context lookup — see `crate::features::terminal::domain`'s
//! module docs for the full widening accounting.

pub use crate::features::sessions::domain::repositories::SessionsRepository;
pub use crate::features::settings::domain::repositories::PreferencesRepository;

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{Space, TabEntry, TermLine};

/// Read access to the attached session's tab/pane world, the two switches the
/// sheet performs, and detach.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait TerminalRepository {
    /// Address `session` from now on, routing through `connection_id` — see
    /// the [module docs](self)' "Which session, and over which connection".
    ///
    /// Synchronous and infallible: it records two strings, it does not talk to
    /// anything. `None`/empty on either means "unknown", which the wire spells
    /// as an empty field (its own documented legacy/fallback routing). The
    /// default is a no-op, so an implementation with no wire behind it — the
    /// in-memory mock — inherits it and stays untouched.
    fn bind(&self, _session: Option<String>, _connection_id: Option<String>) {}

    /// Every space the connected server knows about, in display order, with
    /// the selected one flagged.
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure>;

    /// Every tab in the attached session, in display order, with the active
    /// one flagged.
    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure>;

    /// The scrollback the tab keyed `key` currently shows.
    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure>;

    /// Make the space with [`Space::id`] `space_id` the active one.
    ///
    /// A **view** switch rather than a mutation: which space this connection
    /// is looking at is per-connection state the backend tracks against the
    /// routing token [`bind`](Self::bind) recorded, so it stays available to a
    /// read-only credential.
    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure>;

    /// Move the session's focus to the tab with [`TabEntry::tab_id`] `tab_id`.
    ///
    /// Separate from [`tab_lines`](Self::tab_lines) — that one reads what a
    /// tab shows, this one tells the backend which tab the session is on — so
    /// a screen switching tabs calls both, in that order.
    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure>;

    /// Detach this client from the session with `id`, leaving it running.
    async fn detach_session(&self, id: String) -> Result<(), AppFailure>;
}
