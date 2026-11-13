//! Every method fails with [`AppFailure`]; the workspace verbs default to
//! [`AppFailure::Internal`], so a double that never supports a verb keeps compiling. A session's
//! name is written through `sessions`' repository, never here.

pub use crate::features::sessions::domain::repositories::SessionsRepository;
pub use crate::features::settings::domain::repositories::PreferencesRepository;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{
    AgentEntry, AgentEventEntry, ScrollStep, Space, TabEntry, TermLine,
};

fn unsupported(verb: &str) -> AppFailure {
    AppFailure::Internal(format!("{verb} is not supported by this repository"))
}

/// Called off the UI thread by a real stream, so it must not touch a signal itself.
pub type AgentSink = Box<dyn Fn(AgentEventEntry) + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WatchEnd {
    Open = 0,
    Reconnecting = 1,
    EndedClean = 2,
    EndedUnauthenticated = 3,
    Exhausted = 4,
}

impl WatchEnd {
    pub fn is_ended(self) -> bool {
        matches!(
            self,
            Self::EndedClean | Self::EndedUnauthenticated | Self::Exhausted
        )
    }

    pub fn store(self, cell: &AtomicU8) {
        cell.store(self as u8, Ordering::Release);
    }

    /// An unknown value reads [`Open`](Self::Open).
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

/// Dropping it stops the stream.
pub struct WatchHandle {
    live: Arc<AtomicBool>,
    ended: Arc<AtomicU8>,
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl WatchHandle {
    /// Runs `stop` once, on drop.
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

    /// `false` while a reconnect waits out its backoff, and for good once the subscription ends.
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

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

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait TerminalRepository {
    /// Records the session to address and the routing token the attached stream minted;
    /// `None` or empty means unknown.
    fn bind(&self, _session: Option<String>, _connection_id: Option<String>) {}

    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure>;

    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure>;

    /// A read-only peek that never moves selection. A daemon predating the wire's `space_id`
    /// field answers the focused space instead, undetectably.
    async fn list_tabs_in_space(&self, _space_id: &str) -> Result<Vec<TabEntry>, AppFailure> {
        self.list_tabs().await
    }

    /// Session-wide across all spaces, most urgent first; a backend without agents answers empty.
    async fn list_agents(&self) -> Result<Vec<AgentEntry>, AppFailure> {
        Ok(Vec::new())
    }

    /// An opening [`AgentEventEntry::Snapshot`], then deltas, until the handle drops.
    fn watch_agents(&self, _sink: AgentSink) -> Result<WatchHandle, AppFailure> {
        Err(unsupported("watch_agents"))
    }

    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure>;

    /// Per-connection state keyed on the [`bind`](Self::bind) token. The wire carries no
    /// read-only gate here; the only failure left is a fail-closed `reattach required`.
    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure>;

    /// A tab switch calls [`tab_lines`](Self::tab_lines) and then this, in that order.
    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure>;

    async fn detach_session(&self, id: String) -> Result<(), AppFailure>;

    /// An empty `label` lets the backend choose a name. Mutating.
    async fn create_space(&self, _label: String) -> Result<(), AppFailure> {
        Err(unsupported("create_space"))
    }

    async fn rename_space(&self, _space_id: String, _label: String) -> Result<(), AppFailure> {
        Err(unsupported("rename_space"))
    }

    /// An empty `name` lets the backend choose one. Mutating.
    async fn new_tab(&self, _name: String) -> Result<(), AppFailure> {
        Err(unsupported("new_tab"))
    }

    async fn rename_tab(&self, _tab_id: u64, _name: String) -> Result<(), AppFailure> {
        Err(unsupported("rename_tab"))
    }

    async fn new_pane(&self, _floating: bool) -> Result<(), AppFailure> {
        Err(unsupported("new_pane"))
    }

    /// `is_plugin` picks the wire's id namespace for `pane_id`. Mutating.
    async fn rename_pane(
        &self,
        _pane_id: u32,
        _is_plugin: bool,
        _name: String,
    ) -> Result<(), AppFailure> {
        Err(unsupported("rename_pane"))
    }

    /// Not mutating: a read-only credential may focus a pane on either backend.
    async fn focus_pane(&self, _pane_id: u32, _is_plugin: bool) -> Result<(), AppFailure> {
        Err(unsupported("focus_pane"))
    }

    /// Mutating, refused for a read-only credential. herdr answers `ok: false` by design, which
    /// surfaces as an `Err`.
    async fn toggle_pane_fullscreen(
        &self,
        _pane_id: u32,
        _is_plugin: bool,
    ) -> Result<(), AppFailure> {
        Err(unsupported("toggle_pane_fullscreen"))
    }

    /// Allowed for a read-only credential.
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
