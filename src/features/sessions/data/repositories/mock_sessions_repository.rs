//! In-memory `SessionsRepository` seeded with `dev`, `jumping-peach` and `herdr`; create/rename/kill mutate that set in place.
//! An unknown id or blank name maps to `AppFailure::Internal`; a duplicate name maps to `AppFailure::Network`.
//! A `Mutex<Vec<Session>>` keeps it `Send + Sync`; `MOCK_LATENCY` keeps the async path realistic.

use std::sync::Mutex;
use std::time::Duration;

use crate::agents::{AgentSummary, PREVIEW_AGENT_MARKS, agent_sort_key};
use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{
    CreateSessionParams, LayoutSummary, PaneSummary, RenameRequest, Session, SessionAgent,
    SessionBackend, SessionStatus, TabSummary, ThirdMetric,
};
use crate::features::sessions::domain::repositories::SessionsRepository;

const MOCK_LATENCY: Duration = Duration::from_millis(20);

pub const SEEDED_DEV_ID: &str = "zellij:dev";
pub const SEEDED_PEACH_ID: &str = "zellij:jumping-peach";
pub const SEEDED_HERDR_ID: &str = "herdr:herdr";

pub struct MockSessionsRepository {
    sessions: Mutex<Vec<Session>>,
}

impl MockSessionsRepository {
    pub fn seeded() -> Self {
        Self {
            sessions: Mutex::new(seed()),
        }
    }

    pub fn snapshot(&self) -> Vec<Session> {
        self.lock().clone()
    }

    /// Test seam: adds a pane to the session called `name`; panics when absent.
    pub fn churn(&self, name: &str) {
        let mut sessions = self.lock();
        let session = sessions
            .iter_mut()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("churn: no session named {name:?}"));
        session.layout.panes += 1;
    }

    /// Test seam: sets the agent roll-up of session `id`; panics when absent.
    pub fn seed_agents_for_tests(&self, id: &str, agents: Option<AgentSummary>) {
        let mut sessions = self.lock();
        let session = sessions
            .iter_mut()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("seed_agents_for_tests: no session with id {id:?}"));
        session.agents = agents;
    }

    /// Recovers a poisoned lock so one panicking test does not cascade.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for MockSessionsRepository {
    fn default() -> Self {
        Self::seeded()
    }
}

/// Mirrors the gRPC repository's `<backend>:<name>` id, so a rename changes the id.
fn session_id(backend: SessionBackend, name: &str) -> String {
    format!("{}:{name}", backend.label())
}

fn duplicate(name: &str) -> AppFailure {
    AppFailure::Network(format!("a session named {name:?} already exists"))
}

fn pane(title: &str, cwd: Option<&str>, focused: bool, floating: bool) -> PaneSummary {
    PaneSummary {
        id: 0,
        title: title.to_string(),
        cwd: cwd.map(str::to_string),
        focused,
        floating,
        exited: false,
    }
}

fn tab(index: u32, name: &str, focused: bool, mut panes: Vec<PaneSummary>) -> TabSummary {
    for (id, pane) in (0..).zip(panes.iter_mut()) {
        pane.id = id;
    }
    TabSummary {
        index,
        name: name.to_string(),
        focused,
        has_bell: false,
        panes,
    }
}

fn ringing(tab: TabSummary) -> TabSummary {
    TabSummary {
        has_bell: true,
        ..tab
    }
}

/// Transcribed 1:1 from the design mock, in its order.
fn seed() -> Vec<Session> {
    vec![
        Session {
            id: session_id(SessionBackend::Zellij, "dev"),
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
                preview: vec!["btop".to_string(), "ed@workbench:~".to_string()],
                tabs_detail: vec![
                    tab(
                        0,
                        "main",
                        true,
                        vec![
                            pane("btop", Some("/home/ed"), true, false),
                            pane("ed@workbench:~", Some("/home/ed"), false, false),
                            pane("scratch", None, false, true),
                        ],
                    ),
                    ringing(tab(
                        1,
                        "build",
                        false,
                        vec![
                            pane("cargo watch", Some("/home/ed/muxr"), false, false),
                            pane("tail -f", Some("/var/log"), false, false),
                        ],
                    )),
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
            agents: None,
        },
        Session {
            id: session_id(SessionBackend::Zellij, "jumping-peach"),
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
            agents: None,
        },
        Session {
            id: session_id(SessionBackend::Herdr, "herdr"),
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
            agents: Some(AgentSummary::from_statuses(
                PREVIEW_AGENT_MARKS.iter().copied(),
            )),
        },
    ]
}

fn seed_herdr_agents() -> Vec<SessionAgent> {
    let places = [
        (
            "reviewer",
            0,
            0,
            "Waiting for approval to run the migration",
            4,
        ),
        ("planner", 0, 1, "Finished the refactor plan", 3),
        ("scout", 1, 0, "Reading the build logs", 2),
        ("notes", 1, 1, "Idle since the last summary", 1),
    ];
    let mut rows: Vec<SessionAgent> = PREVIEW_AGENT_MARKS
        .iter()
        .zip(places)
        .map(
            |(&(status, kind), (name, tab_position, pane_id, summary, change_seq))| SessionAgent {
                pane_id,
                tab_position: Some(tab_position),
                space_id: Some("home".to_string()),
                space_name: Some("home".to_string()),
                tab_name: if tab_position == 0 { "work" } else { "notes" }.to_string(),
                kind: kind.to_string(),
                name: Some(name.to_string()),
                display_kind: None,
                status,
                title: None,
                summary: Some(summary.to_string()),
                change_seq,
            },
        )
        .collect();
    rows.sort_by_key(|row| agent_sort_key(row.status, row.change_seq));
    rows
}

fn new_session(name: String, backend: SessionBackend) -> Session {
    let third = match backend {
        SessionBackend::Zellij => ThirdMetric::Clients(0),
        SessionBackend::Herdr => ThirdMetric::Spaces(0),
    };
    Session {
        id: session_id(backend, &name),
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
        agents: None,
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for MockSessionsRepository {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        Ok(self.snapshot())
    }

    async fn list_agents(&self, session_id: String) -> Result<Vec<SessionAgent>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        let agents = self
            .snapshot()
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.agents.as_ref())
            .is_some_and(|summary| !summary.is_empty());
        Ok(if agents && session_id == SEEDED_HERDR_ID {
            seed_herdr_agents()
        } else {
            Vec::new()
        })
    }

    async fn create_session(&self, params: CreateSessionParams) -> Result<Session, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        let name = params.name.trim().to_string();
        if name.is_empty() {
            return Err(AppFailure::Internal(
                "create_session called with a blank name".to_string(),
            ));
        }
        let session = new_session(name, params.backend);
        let mut sessions = self.lock();
        if sessions.iter().any(|s| s.id == session.id) {
            return Err(duplicate(&session.name));
        }
        sessions.push(session.clone());
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
        let Some(backend) = sessions
            .iter()
            .find(|s| s.id == request.id)
            .map(|s| s.backend)
        else {
            return Err(AppFailure::Internal(format!(
                "rename_session: no session {}",
                request.id
            )));
        };
        let id = session_id(backend, &name);
        if sessions.iter().any(|s| s.id == id && s.id != request.id) {
            return Err(duplicate(&name));
        }
        let Some(session) = sessions.iter_mut().find(|s| s.id == request.id) else {
            unreachable!("the id was found above under the same lock");
        };
        session.id = id;
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

    const SEEDED_COUNT: usize = 3;

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

        assert!(sessions[0].status.is_live());
        assert_eq!(sessions[0].backend, SessionBackend::Zellij);
        assert_eq!(sessions[0].layout.panes, 9);
        assert_eq!(sessions[0].layout.tabs, 4);
        assert_eq!(sessions[0].layout.third, ThirdMetric::Clients(0));
        assert_eq!(
            sessions[0].layout.preview,
            vec!["btop".to_string(), "ed@workbench:~".to_string()]
        );

        assert!(!sessions[1].status.is_live());
        assert!(sessions[1].layout.preview.is_empty());
        assert_eq!(sessions[1].layout.third, ThirdMetric::TabsSaved(0));

        assert_eq!(sessions[2].backend, SessionBackend::Herdr);
        assert_eq!(sessions[2].layout.third, ThirdMetric::Spaces(5));
        assert_eq!(sessions[2].layout.panes, 3);
        assert_eq!(sessions[2].layout.tabs, 2);

        assert_eq!(sessions[0].agents, None);
        assert_eq!(sessions[1].agents, None);
        let agents = sessions[2].agents.as_ref().expect("herdr agents");
        assert_eq!((agents.total, agents.blocked, agents.done), (4, 1, 1));
        assert_eq!((agents.working, agents.idle, agents.unknown), (1, 1, 0));
        assert_eq!(agents.kinds, ["claude", "codex", "gemini"]);
    }

    #[test]
    fn seed_agents_for_tests_overrides_one_sessions_summary() {
        let repo = MockSessionsRepository::seeded();
        repo.seed_agents_for_tests(SEEDED_HERDR_ID, None);
        repo.seed_agents_for_tests(
            SEEDED_DEV_ID,
            Some(AgentSummary {
                total: 1,
                idle: 1,
                kinds: vec!["claude".to_string()],
                ..AgentSummary::default()
            }),
        );
        let sessions = repo.snapshot();
        assert_eq!(sessions[2].agents, None);
        assert_eq!(sessions[0].agents.as_ref().map(|a| a.total), Some(1));
    }

    #[test]
    fn create_appends_a_session_with_a_name_derived_id() {
        let repo = MockSessionsRepository::seeded();
        let created = block_on(repo.create_session(params("staging"))).expect("create succeeds");

        assert_eq!(created.name, "staging");
        assert_eq!(created.id, "zellij:staging");
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
    fn rename_changes_the_name_and_mints_the_id_from_it() {
        let repo = MockSessionsRepository::seeded();
        let renamed = block_on(repo.rename_session(RenameRequest {
            id: SEEDED_DEV_ID.to_string(),
            name: "  dev-2  ".to_string(),
        }))
        .expect("rename succeeds");

        assert_eq!(renamed.id, "zellij:dev-2");
        assert_eq!(renamed.name, "dev-2", "the stored name is trimmed");
        assert_eq!(
            repo.snapshot()
                .iter()
                .find(|s| s.id == "zellij:dev-2")
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
        block_on(repo.kill_session(SEEDED_PEACH_ID.to_string())).expect("kill succeeds");

        let sessions = repo.snapshot();
        assert_eq!(sessions.len(), SEEDED_COUNT - 1);
        assert!(sessions.iter().all(|s| s.id != SEEDED_PEACH_ID));
    }

    #[test]
    fn killing_every_session_empties_the_store() {
        let repo = MockSessionsRepository::seeded();
        for id in [SEEDED_DEV_ID, SEEDED_PEACH_ID, SEEDED_HERDR_ID] {
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

    #[test]
    fn the_seeded_id_constants_name_the_seeded_sessions() {
        let ids: Vec<String> = seed().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, [SEEDED_DEV_ID, SEEDED_PEACH_ID, SEEDED_HERDR_ID]);
    }

    #[test]
    fn a_duplicate_backend_and_name_is_rejected_on_create_and_rename() {
        let repo = MockSessionsRepository::seeded();

        let created = block_on(repo.create_session(params("dev"))).expect_err("dev exists");
        assert!(matches!(created, AppFailure::Network(_)));

        let renamed = block_on(repo.rename_session(RenameRequest {
            id: SEEDED_PEACH_ID.to_string(),
            name: "dev".to_string(),
        }))
        .expect_err("dev exists");
        assert!(matches!(renamed, AppFailure::Network(_)));

        assert_eq!(repo.snapshot().len(), SEEDED_COUNT, "nothing changed");
        block_on(repo.rename_session(RenameRequest {
            id: SEEDED_DEV_ID.to_string(),
            name: "dev".to_string(),
        }))
        .expect("renaming a session to its own name is not a duplicate");
    }
}
