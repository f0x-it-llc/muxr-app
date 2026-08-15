//! `terminal` domain repository contract — the seam this feature's use cases
//! depend on and `data` implements, framework-free like the rest of `domain`.
//!
//! [`TerminalRepository`] is the whole contract: read the attached session's
//! spaces/tabs, read one tab's lines, switch space/tab, detach, plus the "panes &
//! tabs" sheet's own mutations. Writing a session's *name* is deliberately **not**
//! here — that belongs to the `sessions` feature's repository, and this screen's
//! rename delegates to it rather than minting a second write path onto one field.
//! Every method fails with the app-wide [`AppFailure`] vocabulary, and every one
//! is **async regardless**, even though the in-memory implementation never awaits,
//! so the real `muxrd` client drops in behind it additively.
//!
//! **Which session, and over which connection.** The read/detach methods take no
//! session id — their signatures are load-bearing for `super::use_cases` — so
//! [`TerminalRepository::bind`] is how a live repository learns which session to
//! address and the per-connection routing token the attached stream minted.
//!
//! [`SessionsRepository`] and [`PreferencesRepository`] are **re-exported** from
//! their owning features rather than re-implemented, so this screen's rename
//! writes through the one store a session's name lives in and the live pane reads
//! the one store `settings` writes; `presentation` reaches both through here
//! rather than naming a sibling's `domain` itself. The workspace verbs each carry
//! a **default implementation** answering [`AppFailure::Internal`], so every bare
//! [`TerminalRepository`] double keeps compiling: reaching a default means a
//! double that never claimed to support the verb was asked to perform it.

pub use crate::features::sessions::domain::repositories::SessionsRepository;
pub use crate::features::settings::domain::repositories::PreferencesRepository;

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{ScrollStep, Space, TabEntry, TermLine};

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
    /// [module docs](self). Synchronous and infallible: it records two strings.
    /// `None`/empty on either means "unknown", which the wire spells as an empty
    /// field. The default is a no-op, so an implementation with no wire behind it
    /// inherits it and stays untouched.
    fn bind(&self, _session: Option<String>, _connection_id: Option<String>) {}

    /// Every space the connected server knows about, in display order, with
    /// the selected one flagged.
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure>;

    /// Every tab in the attached session, in display order, with the active
    /// one flagged.
    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure>;

    /// The scrollback the tab keyed `key` currently shows.
    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure>;

    /// Make the space with [`Space::id`] `space_id` the active one. Which space this
    /// connection is looking at is per-connection state the backend tracks against
    /// the routing token [`bind`](Self::bind) recorded — but the wire still marks
    /// `SwitchSpace` **MUTATING**, and a read-only credential is refused it.
    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure>;

    /// Move the session's focus to the tab with [`TabEntry::tab_id`] `tab_id`.
    /// Separate from [`tab_lines`](Self::tab_lines) — that one reads what a tab
    /// shows, this one tells the backend which tab the session is on — so a screen
    /// switching tabs calls both, in that order.
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

    /// Open a new tab named `name` (the wire's `NewTabReq.tab_name`); an empty
    /// `name` leaves it unnamed, letting the backend choose one — the sheet's
    /// add-tab dialog sends exactly that on a blank confirm, the same "may be
    /// empty" shape [`create_space`](Self::create_space)'s `label` takes. MUTATING.
    async fn new_tab(&self, _name: String) -> Result<(), AppFailure> {
        Err(unsupported("new_tab"))
    }

    /// Rename the tab with [`TabEntry::tab_id`] `tab_id` to `name`. MUTATING.
    async fn rename_tab(&self, _tab_id: u64, _name: String) -> Result<(), AppFailure> {
        Err(unsupported("rename_tab"))
    }

    /// Open a new pane — floating if `floating`, else a focused tiled one.
    /// MUTATING.
    async fn new_pane(&self, _floating: bool) -> Result<(), AppFailure> {
        Err(unsupported("new_pane"))
    }

    /// Rename the pane `pane_id` to `name`. `is_plugin` picks which of the wire's
    /// two id namespaces `pane_id` lives in (see
    /// [`PaneEntry::is_plugin`](super::entities::PaneEntry::is_plugin)). MUTATING.
    async fn rename_pane(
        &self,
        _pane_id: u32,
        _is_plugin: bool,
        _name: String,
    ) -> Result<(), AppFailure> {
        Err(unsupported("rename_pane"))
    }

    /// Focus the pane `pane_id` (`is_plugin` as in
    /// [`rename_pane`](Self::rename_pane)). A **view** switch rather than a
    /// mutation — the wire's own `FocusPane` is the one pane action that is not
    /// MUTATING, so a read-only credential may send it. A backend whose relay is
    /// per-connection (herdr) still drops it silently for such a credential, which
    /// is why `presentation` refuses that combination before it goes out.
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
}
