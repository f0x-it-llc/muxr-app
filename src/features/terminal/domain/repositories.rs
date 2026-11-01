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

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{
    AgentEntry, ScrollStep, Space, TabEntry, TermLine,
};

/// The failure every default verb below answers with — see the [module
/// docs](self)' "Workspace verbs" section for why it is always this shape.
fn unsupported(verb: &str) -> AppFailure {
    AppFailure::Internal(format!("{verb} is not supported by this repository"))
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
            block_on(repo.scroll_pane(1, false, ScrollStep::Up)),
            Err(AppFailure::Internal(_))
        ));
    }

    #[test]
    fn list_tabs_in_space_defaults_to_a_bare_list_tabs_call() {
        let repo = BareRepository;
        assert_eq!(block_on(repo.list_tabs_in_space("s2")), Ok(Vec::new()));
    }
}
