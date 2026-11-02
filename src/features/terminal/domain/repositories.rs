//! `terminal` domain repository contract — the seam `use_cases` depends on and `data`
//! implements, framework-free like the rest of `domain`.
//!
//! [`TerminalRepository`] covers: read the attached session's spaces/tabs, read one tab's
//! lines (or another space's, read-only, via [`list_tabs_in_space`](TerminalRepository::list_tabs_in_space)),
//! switch space/tab, detach, plus the "panes & tabs" sheet's own mutations. Writing a session's
//! *name* is deliberately not here — that belongs to `sessions`' repository, and this screen's
//! rename delegates to it. Every method fails with [`AppFailure`] and is async regardless of
//! whether an implementation ever awaits, so the real `muxrd` client drops in behind it
//! additively.
//!
//! **Which session, which connection.** The read/detach methods take no session id (their
//! signatures are load-bearing for `super::use_cases`) — [`TerminalRepository::bind`] is how a
//! live repository learns which session to address and the per-connection routing token the
//! attached stream minted.
//!
//! [`SessionsRepository`] and [`PreferencesRepository`] are re-exported from their owning
//! features rather than re-implemented, so this screen's rename/live-pane reads go through the
//! one store that owns each. The workspace verbs each default to [`AppFailure::Internal`], so a
//! bare [`TerminalRepository`] double that never claimed to support a verb keeps compiling.

pub use crate::features::sessions::domain::repositories::SessionsRepository;
pub use crate::features::settings::domain::repositories::PreferencesRepository;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{
    AgentEntry, AgentEventEntry, ScrollStep, Space, TabEntry, TermLine,
};

/// The failure every default verb below answers with — see the [module
/// docs](self)' "Workspace verbs" section for why it is always this shape.
fn unsupported(verb: &str) -> AppFailure {
    AppFailure::Internal(format!("{verb} is not supported by this repository"))
}

/// Where [`TerminalRepository::watch_agents`] delivers events. Called off the UI thread by a
/// real stream, so it must not touch a signal itself.
pub type AgentSink = Box<dyn Fn(AgentEventEntry) + Send>;

/// Where a [`WatchHandle`]'s subscription stands — why it stopped, once it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WatchEnd {
    /// A stream is open or opening.
    Open = 0,
    /// A stream dropped and the next one waits out its backoff.
    Reconnecting = 1,
    /// The server ended the stream: nothing more will arrive on this subscription.
    EndedClean = 2,
    /// The server refused the credential.
    EndedUnauthenticated = 3,
    /// The reconnect schedule ran out.
    Exhausted = 4,
}

impl WatchEnd {
    /// Whether the subscription is over for good.
    pub fn is_ended(self) -> bool {
        matches!(
            self,
            Self::EndedClean | Self::EndedUnauthenticated | Self::Exhausted
        )
    }

    /// Record `self` in the cell a [`WatchHandle`] reads.
    pub fn store(self, cell: &AtomicU8) {
        cell.store(self as u8, Ordering::Release);
    }

    /// The state `cell` holds; an unknown value reads [`Open`](Self::Open).
    pub fn load(cell: &AtomicU8) -> Self {
        match cell.load(Ordering::Acquire) {
            1 => Self::Reconnecting,
            2 => Self::EndedClean,
            3 => Self::EndedUnauthenticated,
            4 => Self::Exhausted,
            _ => Self::Open,
        }
    }
}

/// An open [`TerminalRepository::watch_agents`] subscription. **Dropping it stops the stream.**
pub struct WatchHandle {
    live: Arc<AtomicBool>,
    ended: Arc<AtomicU8>,
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl WatchHandle {
    /// A handle whose stream is open while `live` holds, whose [`WatchEnd`] the implementation
    /// keeps in `ended`, and which runs `stop` once on drop.
    pub fn new(
        live: Arc<AtomicBool>,
        ended: Arc<AtomicU8>,
        stop: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            live,
            ended,
            stop: Some(Box::new(stop)),
        }
    }

    /// Whether a stream is delivering right now: `false` while a reconnect waits out its
    /// backoff, and for good once the subscription has given up.
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    /// Where the subscription stands — see [`WatchEnd`].
    pub fn end(&self) -> WatchEnd {
        WatchEnd::load(&self.ended)
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

/// Read access to the attached session's tab/pane world, the two switches the
/// sheet performs, and detach.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait TerminalRepository {
    /// Address `session` from now on, routing through `connection_id` — see the
    /// [module docs](self). Synchronous and infallible; `None`/empty on either means
    /// "unknown" (the wire's own empty-field spelling). Default is a no-op.
    fn bind(&self, _session: Option<String>, _connection_id: Option<String>) {}

    /// Every space the connected server knows about, in display order, with
    /// the selected one flagged.
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure>;

    /// Every tab in the attached session, in display order, with the active
    /// one flagged.
    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure>;

    /// The tabs of the space `space_id` names — a **read-only peek** that never moves
    /// selection, unlike [`switch_space`](Self::switch_space). An old daemon predating the
    /// wire's `space_id` field silently answers the focused space instead, undetectably.
    /// Default: a bare [`list_tabs`](Self::list_tabs) call.
    async fn list_tabs_in_space(&self, _space_id: &str) -> Result<Vec<TabEntry>, AppFailure> {
        self.list_tabs().await
    }

    /// Every AI agent running in the bound session, **session-wide across all its spaces** (not
    /// just the space this connection views), most urgent first. A backend without agents, or
    /// an unbound repository, answers empty rather than failing. Default: empty.
    async fn list_agents(&self) -> Result<Vec<AgentEntry>, AppFailure> {
        Ok(Vec::new())
    }

    /// Subscribe to the bound session's agents: an opening [`AgentEventEntry::Snapshot`], then
    /// deltas, into `sink` until the returned handle drops. Default: unsupported, which leaves
    /// the caller polling [`list_agents`](Self::list_agents).
    fn watch_agents(&self, _sink: AgentSink) -> Result<WatchHandle, AppFailure> {
        Err(unsupported("watch_agents"))
    }

    /// The scrollback the tab keyed `key` currently shows.
    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure>;

    /// Make the space with [`Space::id`] `space_id` the active one. Which space this
    /// connection is looking at is per-connection backend state keyed on the routing
    /// token [`bind`](Self::bind) recorded. The wire's `SwitchSpace` carries no
    /// read-only gate — a read-only credential may call it; the only remaining
    /// failure is a fail-closed `reattach required`.
    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure>;

    /// Move the session's focus to the tab with [`TabEntry::tab_id`] `tab_id`. Separate
    /// from [`tab_lines`](Self::tab_lines) — that reads what a tab shows, this tells the
    /// backend which tab the session is on — a tab switch calls both, in that order.
    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure>;

    /// Detach this client from the session with `id`, leaving it running.
    async fn detach_session(&self, id: String) -> Result<(), AppFailure>;

    // -- Workspace verbs — see the module docs' "Workspace verbs" section ---

    /// Create a new space (herdr workspace); `label` may be empty, letting the
    /// backend choose a name (the wire's `CreateSpaceReq.label`). MUTATING.
    async fn create_space(&self, _label: String) -> Result<(), AppFailure> {
        Err(unsupported("create_space"))
    }

    /// Rename the space with [`Space::id`] `space_id` to `label`. MUTATING.
    async fn rename_space(&self, _space_id: String, _label: String) -> Result<(), AppFailure> {
        Err(unsupported("rename_space"))
    }

    /// Open a new tab named `name` (the wire's `NewTabReq.tab_name`); an empty `name` lets the
    /// backend choose one — same "may be empty" shape as [`create_space`](Self::create_space)'s
    /// `label`. MUTATING.
    async fn new_tab(&self, _name: String) -> Result<(), AppFailure> {
        Err(unsupported("new_tab"))
    }

    /// Rename the tab with [`TabEntry::tab_id`] `tab_id` to `name`. MUTATING.
    async fn rename_tab(&self, _tab_id: u64, _name: String) -> Result<(), AppFailure> {
        Err(unsupported("rename_tab"))
    }

    /// Open a new pane — floating if `floating`, else a focused tiled one. MUTATING.
    async fn new_pane(&self, _floating: bool) -> Result<(), AppFailure> {
        Err(unsupported("new_pane"))
    }

    /// Rename the pane `pane_id` to `name`. `is_plugin` picks which of the wire's two id
    /// namespaces `pane_id` lives in (see
    /// [`PaneEntry::is_plugin`](super::entities::PaneEntry::is_plugin)). MUTATING.
    async fn rename_pane(
        &self,
        _pane_id: u32,
        _is_plugin: bool,
        _name: String,
    ) -> Result<(), AppFailure> {
        Err(unsupported("rename_pane"))
    }

    /// Focus the pane `pane_id` (`is_plugin` as in [`rename_pane`](Self::rename_pane)) — a view
    /// switch, not a mutation: the wire's `FocusPane` is the one pane action not MUTATING, so a
    /// read-only credential may send it on either backend — neither zellij nor herdr gates
    /// this on the read-only credential.
    async fn focus_pane(&self, _pane_id: u32, _is_plugin: bool) -> Result<(), AppFailure> {
        Err(unsupported("focus_pane"))
    }

    /// Toggle zellij pane fullscreen (the no-UI variant, muxrd PR #42) for the pane `pane_id`
    /// (`is_plugin` as in [`rename_pane`](Self::rename_pane)). MUTATING: the server refuses it
    /// for a read-only credential. herdr answers `ok: false` by design, which surfaces as an
    /// `Err`.
    async fn toggle_pane_fullscreen(
        &self,
        _pane_id: u32,
        _is_plugin: bool,
    ) -> Result<(), AppFailure> {
        Err(unsupported("toggle_pane_fullscreen"))
    }

    /// Scroll the pane `pane_id` one wheel notch in `direction` (`is_plugin` as in
    /// [`rename_pane`](Self::rename_pane)). Allowed for a read-only credential, like
    /// [`focus_pane`](Self::focus_pane).
    async fn scroll_pane(
        &self,
        _pane_id: u32,
        _is_plugin: bool,
        _direction: ScrollStep,
    ) -> Result<(), AppFailure> {
        Err(unsupported("scroll_pane"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare implementation that overrides only the six original methods compiles
    /// unmodified, and every workspace verb answers the shared default.
    struct BareRepository;

    #[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
    #[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
    impl TerminalRepository for BareRepository {
        async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
            Ok(Vec::new())
        }

        async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
            Ok(Vec::new())
        }

        async fn tab_lines(&self, _key: String) -> Result<Vec<TermLine>, AppFailure> {
            Ok(Vec::new())
        }

        async fn switch_space(&self, _space_id: String) -> Result<(), AppFailure> {
            Ok(())
        }

        async fn go_to_tab(&self, _tab_id: u64) -> Result<(), AppFailure> {
            Ok(())
        }

        async fn detach_session(&self, _id: String) -> Result<(), AppFailure> {
            Ok(())
        }
    }

    /// Drive one call to completion on a current-thread runtime — no frust
    /// runtime needed, mirroring every other repository test in this crate.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn every_workspace_verb_defaults_to_an_internal_unsupported_failure() {
        let repo = BareRepository;
        assert!(matches!(
            block_on(repo.create_space("label".to_string())),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.rename_space("ws-1".to_string(), "label".to_string())),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.new_tab("name".to_string())),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.rename_tab(1, "name".to_string())),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.new_pane(false)),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.rename_pane(1, false, "name".to_string())),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.focus_pane(1, false)),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.toggle_pane_fullscreen(1, false)),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.scroll_pane(1, false, ScrollStep::Up)),
            Err(AppFailure::Internal(_))
        ));
    }

    #[test]
    fn watch_agents_defaults_to_unsupported() {
        let answer = BareRepository.watch_agents(Box::new(|_| {}));
        assert!(matches!(answer, Err(AppFailure::Internal(_))));
    }

    #[test]
    fn a_watch_handle_reports_its_liveness_its_end_and_stops_once_on_drop() {
        let live = Arc::new(AtomicBool::new(true));
        let ended = Arc::new(AtomicU8::new(0));
        let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&stops);
        let handle = WatchHandle::new(Arc::clone(&live), Arc::clone(&ended), move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        assert!(handle.is_live());
        assert_eq!(handle.end(), WatchEnd::Open);
        live.store(false, Ordering::Release);
        WatchEnd::Reconnecting.store(&ended);
        assert!(!handle.is_live() && !handle.end().is_ended());
        for end in [
            WatchEnd::EndedClean,
            WatchEnd::EndedUnauthenticated,
            WatchEnd::Exhausted,
        ] {
            end.store(&ended);
            assert_eq!(handle.end(), end);
            assert!(end.is_ended());
        }
        drop(handle);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn list_tabs_in_space_defaults_to_a_bare_list_tabs_call() {
        let repo = BareRepository;
        assert_eq!(block_on(repo.list_tabs_in_space("s2")), Ok(Vec::new()));
    }
}
