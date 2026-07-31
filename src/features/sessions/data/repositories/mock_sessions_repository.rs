//! [`MockSessionsRepository`] — the in-memory
//! [`SessionsRepository`] the mock era runs on.
//!
//! Seeds four sessions with deliberately varied tab/pane/client counts (so the
//! cards exercise every shape `docs/redesign.html`'s sessions block draws:
//! running with pane previews, running with many panes, idle with none) and
//! mutates that set in place on create/rename/kill, so the screen proves a real
//! round trip rather than patching its own signal.
//!
//! # Framework-free, and deliberately not `frust`-aware
//!
//! No `frust::` types here (the data-layer rule): the store is a plain
//! [`Mutex<Vec<Session>>`]. The mutex — not a `RefCell` — is what makes this
//! `Send + Sync`, which
//! [`SessionsController`](crate::features::sessions::presentation::controllers::SessionsController)
//! needs to hold it behind an `Arc` under `use_controller`'s bounds.
//!
//! # Failure mapping
//!
//! This is the feature's single failure-mapping site into
//! [`AppFailure`]: an unknown id maps to [`AppFailure::Internal`] (a bug — the
//! screen only ever addresses ids it just rendered) and a blank name to the
//! same, since the dialog that collects one cannot submit blank text. A real
//! gRPC client maps transport errors to [`AppFailure::Network`] and a rejected
//! token to [`AppFailure::Auth`] in exactly this spot.
//!
//! # Latency
//!
//! [`MOCK_LATENCY`] is a short, deliberate `sleep` before every call so the
//! screen's async path (spawn → await → signal write → rebuild) is the one the
//! real client will take, rather than an accidentally-synchronous shortcut.
//! Kept small enough not to be felt (huddle's own repositories use the same
//! device with a 600ms value, because it wants its loading skeletons visible;
//! this screen has none to show).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{
    LayoutSummary, RenameRequest, Session, SessionBackend, SessionStatus,
};
use crate::features::sessions::domain::repositories::SessionsRepository;

/// Simulated round-trip latency on every call — see the [module docs](self).
const MOCK_LATENCY: Duration = Duration::from_millis(20);

/// The creation label a freshly created session gets. A constant rather than
/// "today": this crate has no date dependency, and a fixed string keeps the
/// headless tests deterministic.
const MOCK_CREATED_TODAY: &str = "2026-07-31";

/// An in-memory [`SessionsRepository`]. See the [module docs](self).
pub struct MockSessionsRepository {
    /// The live session set. Mutated in place by create/rename/kill.
    sessions: Mutex<Vec<Session>>,
    /// Mints ids for sessions created at runtime (`s-5`, `s-6`, …), so a
    /// created-then-renamed session keeps one stable key.
    next_id: AtomicU32,
}

impl MockSessionsRepository {
    /// A repository seeded with [`seed`]'s four sessions.
    pub fn seeded() -> Self {
        let sessions = seed();
        let next_id = AtomicU32::new(sessions.len() as u32 + 1);
        Self {
            sessions: Mutex::new(sessions),
            next_id,
        }
    }

    /// The live session set, cloned — a **synchronous** peek for tests and for
    /// this module's own unit tests, never part of the
    /// [`SessionsRepository`] contract (which is what production code speaks).
    pub fn snapshot(&self) -> Vec<Session> {
        self.lock().clone()
    }

    /// The store, with a poisoned lock recovered rather than propagated: a
    /// panicking test must not cascade into every later call on this
    /// repository (`ui::toasts`'s `serial()` uses the same recovery).
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mint the next runtime session id.
    fn mint_id(&self) -> String {
        format!("s-{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for MockSessionsRepository {
    fn default() -> Self {
        Self::seeded()
    }
}

/// The seeded session set: the redesign's three cards (`dev`, `jumping-peach`,
/// `herdr`) plus one more so the list is long enough to scroll on a phone.
fn seed() -> Vec<Session> {
    vec![
        Session {
            id: "s-1".to_string(),
            name: "dev".to_string(),
            created: "2026-07-02".to_string(),
            status: SessionStatus::Running,
            backend: SessionBackend::Zellij,
            layout: LayoutSummary {
                tabs: 4,
                panes: 9,
                clients: 0,
                preview: vec!["btop".to_string(), "ed@precision:~".to_string()],
            },
        },
        Session {
            id: "s-2".to_string(),
            name: "jumping-peach".to_string(),
            created: "2026-06-18".to_string(),
            status: SessionStatus::Resurrectible,
            backend: SessionBackend::Zellij,
            layout: LayoutSummary {
                tabs: 0,
                panes: 0,
                clients: 0,
                preview: Vec::new(),
            },
        },
        Session {
            id: "s-3".to_string(),
            name: "herdr".to_string(),
            created: "2026-07-14".to_string(),
            status: SessionStatus::Running,
            backend: SessionBackend::Herdr,
            layout: LayoutSummary {
                tabs: 2,
                panes: 3,
                clients: 1,
                preview: vec![
                    "\u{b7}\u{b7}\u{b7}".to_string(),
                    "\u{b7}\u{b7}\u{b7}".to_string(),
                ],
            },
        },
        Session {
            id: "s-4".to_string(),
            name: "minefit-api".to_string(),
            created: "2026-05-30".to_string(),
            status: SessionStatus::Running,
            backend: SessionBackend::Zellij,
            layout: LayoutSummary {
                tabs: 1,
                panes: 2,
                clients: 2,
                preview: vec!["cargo watch".to_string()],
            },
        },
    ]
}

/// A session created at runtime: empty layout, today's label, running.
fn new_session(id: String, name: String) -> Session {
    Session {
        id,
        name,
        created: MOCK_CREATED_TODAY.to_string(),
        status: SessionStatus::Running,
        backend: SessionBackend::Zellij,
        layout: LayoutSummary {
            tabs: 1,
            panes: 1,
            clients: 0,
            preview: Vec::new(),
        },
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for MockSessionsRepository {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        Ok(self.snapshot())
    }

    async fn create_session(&self, name: String) -> Result<Session, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(AppFailure::Internal(
                "create_session called with a blank name".to_string(),
            ));
        }
        let session = new_session(self.mint_id(), name);
        self.lock().push(session.clone());
        Ok(session)
    }

    async fn rename_session(&self, request: RenameRequest) -> Result<Session, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        let name = request.name.trim().to_string();
        if name.is_empty() {
            return Err(AppFailure::Internal(
                "rename_session called with a blank name".to_string(),
            ));
        }
        let mut sessions = self.lock();
        let Some(session) = sessions.iter_mut().find(|s| s.id == request.id) else {
            return Err(AppFailure::Internal(format!(
                "rename_session: no session {}",
                request.id
            )));
        };
        session.name = name;
        Ok(session.clone())
    }

    async fn kill_session(&self, id: String) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        let mut sessions = self.lock();
        let before = sessions.len();
        sessions.retain(|s| s.id != id);
        if sessions.len() == before {
            return Err(AppFailure::Internal(format!(
                "kill_session: no session {id}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive one repository call to completion on a current-thread runtime —
    /// these are pure data-layer tests, with no frust runtime in play.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn the_seed_has_four_sessions_with_varied_layouts() {
        let repo = MockSessionsRepository::seeded();
        let sessions = block_on(repo.list_sessions()).expect("the mock never fails to list");

        assert_eq!(sessions.len(), 4);
        let tabs: Vec<u32> = sessions.iter().map(|s| s.layout.tabs).collect();
        let panes: Vec<u32> = sessions.iter().map(|s| s.layout.panes).collect();
        assert!(
            tabs.iter().collect::<std::collections::HashSet<_>>().len() > 1,
            "the seeded tab counts vary: {tabs:?}"
        );
        assert!(
            panes.iter().collect::<std::collections::HashSet<_>>().len() > 1,
            "the seeded pane counts vary: {panes:?}"
        );
        assert!(
            sessions.iter().any(|s| !s.status.is_live()),
            "at least one seeded session is idle, so the dimmed card is reachable"
        );
    }

    #[test]
    fn create_appends_a_session_with_a_fresh_id() {
        let repo = MockSessionsRepository::seeded();
        let created =
            block_on(repo.create_session("staging".to_string())).expect("create succeeds");

        assert_eq!(created.name, "staging");
        let sessions = repo.snapshot();
        assert_eq!(sessions.len(), 5);
        assert_eq!(
            sessions.last().map(|s| s.id.as_str()),
            Some(created.id.as_str())
        );
        assert!(
            sessions.iter().filter(|s| s.id == created.id).count() == 1,
            "the minted id is unique"
        );
    }

    #[test]
    fn create_rejects_a_blank_name() {
        let repo = MockSessionsRepository::seeded();
        let failure = block_on(repo.create_session("   ".to_string())).expect_err("blank rejected");

        assert!(matches!(failure, AppFailure::Internal(_)));
        assert_eq!(repo.snapshot().len(), 4, "nothing was appended");
    }

    #[test]
    fn rename_changes_the_name_and_keeps_the_id() {
        let repo = MockSessionsRepository::seeded();
        let renamed = block_on(repo.rename_session(RenameRequest {
            id: "s-1".to_string(),
            name: "  dev-2  ".to_string(),
        }))
        .expect("rename succeeds");

        assert_eq!(renamed.id, "s-1");
        assert_eq!(renamed.name, "dev-2", "the stored name is trimmed");
        assert_eq!(
            repo.snapshot()
                .iter()
                .find(|s| s.id == "s-1")
                .map(|s| s.name.clone()),
            Some("dev-2".to_string())
        );
    }

    #[test]
    fn rename_of_an_unknown_session_fails_without_mutating() {
        let repo = MockSessionsRepository::seeded();
        let failure = block_on(repo.rename_session(RenameRequest {
            id: "nope".to_string(),
            name: "x".to_string(),
        }))
        .expect_err("an unknown id is a bug, and reports as one");

        assert!(matches!(failure, AppFailure::Internal(_)));
        assert_eq!(repo.snapshot().len(), 4);
    }

    #[test]
    fn kill_removes_exactly_one_session() {
        let repo = MockSessionsRepository::seeded();
        block_on(repo.kill_session("s-2".to_string())).expect("kill succeeds");

        let sessions = repo.snapshot();
        assert_eq!(sessions.len(), 3);
        assert!(sessions.iter().all(|s| s.id != "s-2"));
    }

    #[test]
    fn killing_every_session_empties_the_store() {
        let repo = MockSessionsRepository::seeded();
        for id in ["s-1", "s-2", "s-3", "s-4"] {
            block_on(repo.kill_session(id.to_string())).expect("kill succeeds");
        }

        assert!(repo.snapshot().is_empty(), "the empty state is reachable");
    }

    #[test]
    fn kill_of_an_unknown_session_fails() {
        let repo = MockSessionsRepository::seeded();
        let failure = block_on(repo.kill_session("nope".to_string())).expect_err("unknown id");

        assert!(matches!(failure, AppFailure::Internal(_)));
        assert_eq!(repo.snapshot().len(), 4);
    }
}
