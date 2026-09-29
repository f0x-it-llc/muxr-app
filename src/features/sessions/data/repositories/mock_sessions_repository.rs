//! [`MockSessionsRepository`] — the in-memory
//! [`SessionsRepository`] the mock era runs on.
//!
//! Seeds **exactly** the three sessions `docs/redesign.html`'s sessions block
//! draws — `dev` (running zellij, two pane pills, `◎ 0 clients`),
//! `jumping-peach` (idle resurrectible, no pills, `0 tabs saved`) and `herdr`
//! (running herdr, `◆ 5 spaces`) — so the screen and the mock are the same
//! picture, and every card shape the redesign shows is reachable. It mutates
//! that set in place on create/rename/kill, so the screen proves a real round
//! trip rather than patching its own signal.
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
//! this screen's rain is clocked by the settle edge, not by this delay).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{
    CreateSessionParams, LayoutSummary, PaneSummary, RenameRequest, Session, SessionBackend,
    SessionStatus, TabSummary, ThirdMetric,
};
use crate::features::sessions::domain::repositories::SessionsRepository;

/// Simulated round-trip latency on every call — see the [module docs](self).
const MOCK_LATENCY: Duration = Duration::from_millis(20);

/// An in-memory [`SessionsRepository`]. See the [module docs](self).
pub struct MockSessionsRepository {
    /// The live session set. Mutated in place by create/rename/kill.
    sessions: Mutex<Vec<Session>>,
    /// Mints ids for sessions created at runtime (`s-5`, `s-6`, …), so a
    /// created-then-renamed session keeps one stable key.
    next_id: AtomicU32,
}

impl MockSessionsRepository {
    /// A repository seeded with [`seed`]'s three sessions.
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

/// A terminal pane fixture.
fn pane(title: &str, cwd: Option<&str>, focused: bool, floating: bool) -> PaneSummary {
    PaneSummary {
        title: title.to_string(),
        cwd: cwd.map(str::to_string),
        focused,
        floating,
        exited: false,
    }
}

/// A tab fixture.
fn tab(index: u32, name: &str, focused: bool, panes: Vec<PaneSummary>) -> TabSummary {
    TabSummary {
        index,
        name: name.to_string(),
        focused,
        has_bell: false,
        panes,
    }
}

/// The seeded session set — `docs/redesign.html`'s three cards, transcribed
/// 1:1 (counts, pane previews and all), in the order it draws them.
fn seed() -> Vec<Session> {
    vec![
        Session {
            id: "s-1".to_string(),
            name: "dev".to_string(),
            status: SessionStatus::Running,
            backend: SessionBackend::Zellij,
            age_secs: 3 * 3600,
            has_bell: true,
            is_current: true,
            layout: LayoutSummary {
                tabs: 4,
                panes: 9,
                third: ThirdMetric::Clients(0),
                preview: vec!["btop".to_string(), "ed@precision:~".to_string()],
                tabs_detail: vec![
                    tab(
                        0,
                        "main",
                        true,
                        vec![
                            pane("btop", Some("/home/ed"), true, false),
                            pane("ed@precision:~", Some("/home/ed"), false, false),
                            pane("scratch", None, false, true),
                        ],
                    ),
                    tab(
                        1,
                        "build",
                        false,
                        vec![
                            pane("cargo watch", Some("/home/ed/muxr"), false, false),
                            pane("tail -f", Some("/var/log"), false, false),
                        ],
                    ),
                    tab(
                        2,
                        "edit",
                        false,
                        vec![
                            pane("hx", Some("/home/ed/muxr"), false, false),
                            pane("shell", Some("/home/ed/muxr"), false, false),
                        ],
                    ),
                    tab(
                        3,
                        "logs",
                        false,
                        vec![
                            pane("journalctl", None, false, false),
                            pane("shell", None, false, false),
                        ],
                    ),
                ],
                space: None,
            },
        },
        Session {
            id: "s-2".to_string(),
            name: "jumping-peach".to_string(),
            status: SessionStatus::Resurrectible,
            backend: SessionBackend::Zellij,
            age_secs: 2 * 24 * 3600,
            has_bell: false,
            is_current: false,
            layout: LayoutSummary {
                tabs: 0,
                panes: 0,
                third: ThirdMetric::TabsSaved(0),
                preview: Vec::new(),
                tabs_detail: Vec::new(),
                space: None,
            },
        },
        Session {
            id: "s-3".to_string(),
            name: "herdr".to_string(),
            status: SessionStatus::Running,
            backend: SessionBackend::Herdr,
            age_secs: 45 * 60,
            has_bell: false,
            is_current: false,
            layout: LayoutSummary {
                tabs: 2,
                panes: 3,
                third: ThirdMetric::Spaces(5),
                preview: vec![
                    "\u{b7}\u{b7}\u{b7}".to_string(),
                    "\u{b7}\u{b7}\u{b7}".to_string(),
                ],
                tabs_detail: vec![
                    tab(
                        0,
                        "work",
                        true,
                        vec![
                            pane("\u{b7}\u{b7}\u{b7}", None, true, false),
                            pane("\u{b7}\u{b7}\u{b7}", None, false, false),
                        ],
                    ),
                    tab(
                        1,
                        "notes",
                        false,
                        vec![pane("\u{b7}\u{b7}\u{b7}", None, false, false)],
                    ),
                ],
                space: Some("home".to_string()),
            },
        },
    ]
}

/// A session created at runtime: one empty tab, running, no clients yet.
fn new_session(id: String, name: String, backend: SessionBackend) -> Session {
    let third = match backend {
        SessionBackend::Zellij => ThirdMetric::Clients(0),
        SessionBackend::Herdr => ThirdMetric::Spaces(0),
    };
    Session {
        id,
        name,
        status: SessionStatus::Running,
        backend,
        age_secs: 0,
        has_bell: false,
        is_current: false,
        layout: LayoutSummary {
            tabs: 1,
            panes: 1,
            third,
            preview: Vec::new(),
            tabs_detail: vec![tab(
                0,
                "Tab #1",
                true,
                vec![pane("shell", None, true, false)],
            )],
            space: None,
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

    async fn create_session(&self, params: CreateSessionParams) -> Result<Session, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        let name = params.name.trim().to_string();
        if name.is_empty() {
            return Err(AppFailure::Internal(
                "create_session called with a blank name".to_string(),
            ));
        }
        let session = new_session(self.mint_id(), name, params.backend);
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

    /// How many sessions [`seed`] ships — the redesign's three cards.
    const SEEDED_COUNT: usize = 3;

    /// Drive one repository call to completion on a current-thread runtime —
    /// these are pure data-layer tests, with no frust runtime in play.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    fn params(name: &str) -> CreateSessionParams {
        CreateSessionParams {
            name: name.to_string(),
            backend: SessionBackend::Zellij,
        }
    }

    #[test]
    fn the_seed_drives_the_detail_states() {
        let sessions = seed();
        let dev = sessions[0].layout.focused_tab().expect("a focused tab");
        assert_eq!(dev.panes.len(), 3);
        assert!(dev.panes.iter().any(|p| p.floating));
        assert_eq!(sessions[0].layout.tabs_detail.len(), 4);
        assert!(sessions[1].layout.tabs_detail.is_empty());
        assert_eq!(sessions[2].backend, SessionBackend::Herdr);
    }

    #[test]
    fn the_seed_is_the_redesigns_three_cards_verbatim() {
        let repo = MockSessionsRepository::seeded();
        let sessions = block_on(repo.list_sessions()).expect("the mock never fails to list");

        let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["dev", "jumping-peach", "herdr"]);

        // Card 1 — running zellij, two pane pills, a client count.
        assert!(sessions[0].status.is_live());
        assert_eq!(sessions[0].backend, SessionBackend::Zellij);
        assert_eq!(sessions[0].layout.panes, 9);
        assert_eq!(sessions[0].layout.tabs, 4);
        assert_eq!(sessions[0].layout.third, ThirdMetric::Clients(0));
        assert_eq!(
            sessions[0].layout.preview,
            vec!["btop".to_string(), "ed@precision:~".to_string()]
        );

        // Card 2 — idle: no pills at all, and a saved-tab count instead.
        assert!(!sessions[1].status.is_live());
        assert!(sessions[1].layout.preview.is_empty());
        assert_eq!(sessions[1].layout.third, ThirdMetric::TabsSaved(0));

        // Card 3 — herdr, which counts spaces rather than clients.
        assert_eq!(sessions[2].backend, SessionBackend::Herdr);
        assert_eq!(sessions[2].layout.third, ThirdMetric::Spaces(5));
        assert_eq!(sessions[2].layout.panes, 3);
        assert_eq!(sessions[2].layout.tabs, 2);
    }

    #[test]
    fn create_appends_a_session_with_a_fresh_id() {
        let repo = MockSessionsRepository::seeded();
        let created = block_on(repo.create_session(params("staging"))).expect("create succeeds");

        assert_eq!(created.name, "staging");
        let sessions = repo.snapshot();
        assert_eq!(sessions.len(), SEEDED_COUNT + 1);
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
        let failure = block_on(repo.create_session(params("   "))).expect_err("blank rejected");

        assert!(matches!(failure, AppFailure::Internal(_)));
        assert_eq!(repo.snapshot().len(), SEEDED_COUNT, "nothing was appended");
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
        assert_eq!(repo.snapshot().len(), SEEDED_COUNT);
    }

    #[test]
    fn kill_removes_exactly_one_session() {
        let repo = MockSessionsRepository::seeded();
        block_on(repo.kill_session("s-2".to_string())).expect("kill succeeds");

        let sessions = repo.snapshot();
        assert_eq!(sessions.len(), SEEDED_COUNT - 1);
        assert!(sessions.iter().all(|s| s.id != "s-2"));
    }

    #[test]
    fn killing_every_session_empties_the_store() {
        let repo = MockSessionsRepository::seeded();
        for id in ["s-1", "s-2", "s-3"] {
            block_on(repo.kill_session(id.to_string())).expect("kill succeeds");
        }

        assert!(repo.snapshot().is_empty(), "the empty state is reachable");
    }

    #[test]
    fn kill_of_an_unknown_session_fails() {
        let repo = MockSessionsRepository::seeded();
        let failure = block_on(repo.kill_session("nope".to_string())).expect_err("unknown id");

        assert!(matches!(failure, AppFailure::Internal(_)));
        assert_eq!(repo.snapshot().len(), SEEDED_COUNT);
    }
}
