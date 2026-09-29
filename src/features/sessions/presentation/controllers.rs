use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use clean_signals::{ControllerCore, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{
    CreateSessionParams, RenameRequest, Session, SessionBackend,
};
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::features::sessions::domain::use_cases::{
    CreateSession, KillSession, ListSessions, RenameSession,
};

/// The sentinel [`SessionsController::resume_at`] holds while polling is
/// running — no real navigator depth ever reaches it.
const NOT_PAUSED: usize = usize::MAX;

pub const POLL_PERIOD_SECS: u32 = 3;

pub struct SessionsController {
    core: ControllerCore<AppFailure>,
    list: ListSessions,
    create: CreateSession,
    rename: RenameSession,
    kill: KillSession,
    pub sessions: RwSignal<Vec<Session>>,
    pub loaded: RwSignal<bool>,
    /// Signals when the first `refresh()` has settled (completed, whether
    /// success or failure), releasing the overlay that holds the loading rain.
    pub first_load_settled: RwSignal<bool>,
    /// The Tablet detail's selection, keyed by [`Session::id`] — cleared by a
    /// load whose set no longer carries that id.
    pub selected: RwSignal<Option<String>>,
    resume_at: AtomicUsize,
    polls: AtomicU32,
    poll_in_flight: AtomicBool,
    generation: AtomicU64,
}

impl SessionsController {
    pub fn new(repository: Arc<dyn SessionsRepository + Send + Sync>) -> Self {
        Self {
            core: ControllerCore::new(),
            list: ListSessions::new(Arc::clone(&repository)),
            create: CreateSession::new(Arc::clone(&repository)),
            rename: RenameSession::new(Arc::clone(&repository)),
            kill: KillSession::new(repository),
            sessions: RwSignal::new(Vec::new()),
            loaded: RwSignal::new(false),
            first_load_settled: RwSignal::new(false),
            selected: RwSignal::new(None),
            resume_at: AtomicUsize::new(NOT_PAUSED),
            polls: AtomicU32::new(0),
            poll_in_flight: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }

    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    pub async fn refresh(&self) {
        let my_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let result = self
            .core
            .run(&self.list, NoParams, RunOptions::default())
            .await;
        if self.generation.load(Ordering::SeqCst) != my_generation {
            return;
        }
        if let Ok(sessions) = result {
            self.forget_vanished_selection(&sessions);
            self.sessions.set(sessions);
            self.loaded.set(true);
        }
        self.first_load_settled.set(true);
    }

    pub async fn refresh_from_poll(&self) {
        struct ReleasePollClaim<'a>(&'a AtomicBool);
        impl Drop for ReleasePollClaim<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _release = ReleasePollClaim(&self.poll_in_flight);
        self.refresh().await;
    }

    /// Re-runs a failed first load; its success sets `loaded`, the gate
    /// [`take_poll_tick`](Self::take_poll_tick) reads, so the poll re-arms.
    pub async fn retry_first_load(&self) {
        if !self.loaded.get_untracked() {
            self.refresh().await;
        }
    }

    pub fn select(&self, id: &str) {
        if self.selected.get_untracked().as_deref() != Some(id) {
            self.selected.set(Some(id.to_string()));
        }
    }

    fn forget_vanished_selection(&self, sessions: &[Session]) {
        let vanished = self
            .selected
            .get_untracked()
            .is_some_and(|id| !sessions.iter().any(|s| s.id == id));
        if vanished {
            self.selected.set(None);
        }
    }

    pub async fn create(&self, name: String) {
        let params = CreateSessionParams {
            name,
            backend: SessionBackend::Zellij,
        };
        if self
            .core
            .run(&self.create, params, RunOptions::default())
            .await
            .is_ok()
        {
            self.refresh().await;
        }
    }

    pub async fn rename(&self, id: String, name: String) {
        if self
            .core
            .run(
                &self.rename,
                RenameRequest { id, name },
                RunOptions::default(),
            )
            .await
            .is_ok()
        {
            self.refresh().await;
        }
    }

    pub async fn kill(&self, id: String) {
        if self
            .core
            .run(&self.kill, id, RunOptions::default())
            .await
            .is_ok()
        {
            self.refresh().await;
        }
    }

    /// The session with `id` in the last loaded set, if it is still there —
    /// how an action closure (which captured only an id) recovers the current
    /// name to seed the rename dialog with.
    pub fn session(&self, id: &str) -> Option<Session> {
        self.sessions
            .get_untracked()
            .into_iter()
            .find(|s| s.id == id)
    }

    pub fn take_poll_tick(&self) -> bool {
        let poll = self.is_polling()
            && self.loaded.get_untracked()
            && !self.poll_in_flight.swap(true, Ordering::SeqCst);
        if poll {
            self.polls.fetch_add(1, Ordering::Relaxed);
        }
        poll
    }

    /// Whether the mock poll is currently running (i.e. nothing this screen
    /// pushed is still on top of it).
    pub fn is_polling(&self) -> bool {
        self.resume_at.load(Ordering::Relaxed) == NOT_PAUSED
    }

    /// Poll ticks actually taken so far — observability for the pause's tests,
    /// never read by the UI.
    pub fn polls_taken(&self) -> u32 {
        self.polls.load(Ordering::Relaxed)
    }

    /// Pause the poll until the navigator stack is back at `depth` — called by
    /// the screen with `nav.depth()` *before* it pushes anything. Repeated
    /// calls keep the shallowest mark, so a dialog opened over an already
    /// paused screen doesn't shorten the pause.
    pub fn pause_until_depth(&self, depth: usize) {
        self.resume_at.fetch_min(depth, Ordering::Relaxed);
    }

    /// Observe the live navigator depth (called from the screen's `build`) and
    /// resume the poll once the stack is back at — or below — the mark
    /// [`pause_until_depth`](Self::pause_until_depth) set.
    pub fn observe_depth(&self, depth: usize) {
        let mark = self.resume_at.load(Ordering::Relaxed);
        if mark != NOT_PAUSED && depth <= mark {
            self.resume_at.store(NOT_PAUSED, Ordering::Relaxed);
        }
    }
}

impl AsRef<ControllerCore<AppFailure>> for SessionsController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::sessions::data::repositories::MockSessionsRepository;
    use crate::features::sessions::domain::entities::{
        CreateSessionParams, LayoutSummary, SessionStatus, ThirdMetric,
    };

    /// How many sessions the mock repository seeds (the redesign's three
    /// cards) — named here so a reseed moves one constant, not six literals.
    const SEEDED_COUNT: usize = 3;

    /// The test-wiring composition root: the mock repository behind the domain
    /// trait object. `#[cfg(test)]`-only wiring, so `di.rs` stays the only
    /// production site where presentation meets `data` (huddle's convention).
    fn controller() -> SessionsController {
        SessionsController::new(Arc::new(MockSessionsRepository::seeded()))
    }

    /// Drive a controller call to completion on a current-thread runtime.
    /// These tests exercise the controller's own logic, with no frust
    /// executor/owner in play (`RwSignal`s work fine outside one).
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn refresh_loads_the_seeded_sessions_and_flips_loaded() {
        let c = controller();
        assert!(!c.loaded.get_untracked(), "nothing is loaded up front");

        block_on(c.refresh());

        assert_eq!(c.sessions.get_untracked().len(), SEEDED_COUNT);
        assert!(c.loaded.get_untracked());
    }

    #[test]
    fn create_round_trips_through_the_repository() {
        let c = controller();
        block_on(c.refresh());

        block_on(c.create("staging".to_string()));

        let names: Vec<String> = c
            .sessions
            .get_untracked()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names.len(), SEEDED_COUNT + 1);
        assert!(names.contains(&"staging".to_string()), "{names:?}");
    }

    #[test]
    fn rename_round_trips_and_keeps_the_list_length() {
        let c = controller();
        block_on(c.refresh());
        let id = c.sessions.get_untracked()[0].id.clone();

        block_on(c.rename(id.clone(), "dev-2".to_string()));

        assert_eq!(c.sessions.get_untracked().len(), SEEDED_COUNT);
        assert_eq!(c.session(&id).map(|s| s.name), Some("dev-2".to_string()));
    }

    #[test]
    fn kill_removes_the_session_and_emptying_the_list_is_reachable() {
        let c = controller();
        block_on(c.refresh());

        for session in c.sessions.get_untracked() {
            block_on(c.kill(session.id));
        }

        assert!(c.sessions.get_untracked().is_empty());
        assert!(c.loaded.get_untracked(), "…and it is a *loaded* empty list");
    }

    #[test]
    fn a_failing_call_leaves_the_list_alone() {
        let c = controller();
        block_on(c.refresh());
        let before = c.sessions.get_untracked();

        // No such session — the mock maps an unknown id to `Internal`.
        block_on(c.kill("nope".to_string()));

        assert_eq!(c.sessions.get_untracked(), before);
    }

    #[test]
    fn the_poll_runs_only_once_a_first_load_has_landed() {
        let c = controller();
        assert!(!c.take_poll_tick(), "no poll before the first load");

        block_on(c.refresh());

        assert!(c.take_poll_tick());
        assert_eq!(c.polls_taken(), 1);
    }

    #[test]
    fn a_push_pauses_the_poll_until_the_stack_comes_back() {
        let c = controller();
        block_on(c.refresh());
        assert!(c.is_polling());

        // The screen sits at depth 1 and pushes something over itself.
        c.pause_until_depth(1);
        assert!(!c.is_polling(), "the poll pauses for the pushed page");
        assert!(!c.take_poll_tick());
        assert_eq!(c.polls_taken(), 0, "a skipped tick is not counted");

        // Still covered: a build at depth 2 must not resume it.
        c.observe_depth(2);
        assert!(!c.is_polling());

        // Popped back: the next build resumes it.
        c.observe_depth(1);
        assert!(c.is_polling());
        assert!(c.take_poll_tick());
    }

    #[test]
    fn nested_pushes_keep_the_shallowest_resume_mark() {
        let c = controller();
        block_on(c.refresh());

        c.pause_until_depth(1);
        // A dialog opened *over* the pushed page must not shorten the pause.
        c.pause_until_depth(2);
        c.observe_depth(2);
        assert!(!c.is_polling(), "still covered by the first push");

        c.observe_depth(1);
        assert!(c.is_polling());
    }

    #[test]
    fn a_selection_survives_a_rename_and_a_reload() {
        let c = controller();
        block_on(c.refresh());
        let id = c.sessions.get_untracked()[0].id.clone();
        c.select(&id);

        block_on(c.rename(id.clone(), "dev-2".to_string()));
        block_on(c.refresh());

        assert_eq!(c.selected.get_untracked(), Some(id));
    }

    #[test]
    fn killing_the_selected_session_clears_the_selection() {
        let c = controller();
        block_on(c.refresh());
        let id = c.sessions.get_untracked()[1].id.clone();
        c.select(&id);

        block_on(c.kill(id));

        assert_eq!(c.selected.get_untracked(), None);
    }

    #[test]
    fn a_retry_after_a_failed_first_load_rearms_the_poll() {
        let c = SessionsController::new(Arc::new(FailOnceSessionsRepository::default()));
        block_on(c.refresh());
        assert!(!c.take_poll_tick(), "no poll after a failed first load");

        block_on(c.retry_first_load());

        assert!(c.loaded.get_untracked());
        assert_eq!(c.sessions.get_untracked().len(), SEEDED_COUNT);
        assert!(c.take_poll_tick(), "the successful retry re-arms the poll");
    }

    /// Fails its first `list_sessions`, then answers like the seeded mock.
    #[derive(Default)]
    struct FailOnceSessionsRepository {
        calls: AtomicUsize,
        inner: MockSessionsRepository,
    }

    #[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
    #[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
    impl SessionsRepository for FailOnceSessionsRepository {
        async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(AppFailure::Network("first load".to_string()));
            }
            self.inner.list_sessions().await
        }

        async fn create_session(&self, params: CreateSessionParams) -> Result<Session, AppFailure> {
            self.inner.create_session(params).await
        }

        async fn rename_session(&self, request: RenameRequest) -> Result<Session, AppFailure> {
            self.inner.rename_session(request).await
        }

        async fn kill_session(&self, id: String) -> Result<(), AppFailure> {
            self.inner.kill_session(id).await
        }
    }

    /// A test repository whose list_sessions returns an error.
    struct FailingSessionsRepository;

    #[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
    #[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
    impl SessionsRepository for FailingSessionsRepository {
        async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
            Err(AppFailure::Network("simulated network error".to_string()))
        }

        async fn create_session(
            &self,
            _params: CreateSessionParams,
        ) -> Result<Session, AppFailure> {
            Err(AppFailure::Internal("not implemented".to_string()))
        }

        async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
            Err(AppFailure::Internal("not implemented".to_string()))
        }

        async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
            Err(AppFailure::Internal("not implemented".to_string()))
        }
    }

    #[test]
    fn refresh_flips_both_loaded_and_first_load_settled() {
        let c = controller();
        assert!(!c.loaded.get_untracked());
        assert!(!c.first_load_settled.get_untracked());

        block_on(c.refresh());

        assert!(c.loaded.get_untracked());
        assert!(c.first_load_settled.get_untracked());
    }

    #[test]
    fn failed_first_load_leaves_loaded_false_but_flips_first_load_settled() {
        let c = SessionsController::new(Arc::new(FailingSessionsRepository));
        assert!(!c.loaded.get_untracked());
        assert!(!c.first_load_settled.get_untracked());

        block_on(c.refresh());

        assert!(!c.loaded.get_untracked(), "loaded stays false on error");
        assert!(c.sessions.get_untracked().is_empty());
        assert!(
            c.first_load_settled.get_untracked(),
            "first_load_settled flips to true"
        );
    }

    #[test]
    fn a_second_refresh_leaves_first_load_settled_true() {
        let c = controller();

        block_on(c.refresh());
        assert!(c.first_load_settled.get_untracked());

        block_on(c.refresh());

        assert!(
            c.first_load_settled.get_untracked(),
            "still true after second refresh"
        );
    }

    struct GatedSessionsRepository {
        calls: AtomicUsize,
        gates: Vec<(tokio::sync::Notify, tokio::sync::Notify)>,
    }

    impl GatedSessionsRepository {
        fn new(gated_calls: usize) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                gates: (0..gated_calls)
                    .map(|_| (tokio::sync::Notify::new(), tokio::sync::Notify::new()))
                    .collect(),
            }
        }

        async fn wait_for_call(&self, call: usize) {
            self.gates[call].0.notified().await;
        }

        fn release_call(&self, call: usize) {
            self.gates[call].1.notify_one();
        }
    }

    fn gated_session(call: usize) -> Session {
        Session {
            id: format!("race-{call}"),
            name: format!("call-{call}"),
            status: SessionStatus::Running,
            backend: SessionBackend::Zellij,
            age_secs: 0,
            has_bell: false,
            is_current: false,
            layout: LayoutSummary {
                tabs: 0,
                panes: 0,
                third: ThirdMetric::Clients(0),
                preview: Vec::new(),
                tabs_detail: Vec::new(),
                space: None,
            },
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
    #[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
    impl SessionsRepository for GatedSessionsRepository {
        async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some((started, release)) = self.gates.get(call) {
                started.notify_one();
                release.notified().await;
            }
            Ok(vec![gated_session(call)])
        }

        async fn create_session(
            &self,
            _params: CreateSessionParams,
        ) -> Result<Session, AppFailure> {
            Err(AppFailure::Internal("not implemented".to_string()))
        }

        async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
            Err(AppFailure::Internal("not implemented".to_string()))
        }

        async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
            Err(AppFailure::Internal("not implemented".to_string()))
        }
    }

    #[test]
    fn a_superseded_refresh_does_not_overwrite_the_winning_generation() {
        let repo = Arc::new(GatedSessionsRepository::new(1));
        let c =
            SessionsController::new(Arc::clone(&repo) as Arc<dyn SessionsRepository + Send + Sync>);

        let after_second = block_on(async {
            let refresh1 = c.refresh();
            let refresh2 = c.refresh();
            let orchestrate = async {
                repo.wait_for_call(0).await;
                assert!(c.first_load_settled.get_untracked());
                let after_second = c.sessions.get_untracked();
                repo.release_call(0);
                after_second
            };
            let (_, _, after_second) = tokio::join!(refresh1, refresh2, orchestrate);
            after_second
        });

        assert_eq!(after_second, vec![gated_session(1)]);
        assert_eq!(c.sessions.get_untracked(), after_second);
    }

    #[test]
    fn a_superseded_refresh_that_finishes_before_the_winner_does_not_settle_yet() {
        let repo = Arc::new(GatedSessionsRepository::new(2));
        let c =
            SessionsController::new(Arc::clone(&repo) as Arc<dyn SessionsRepository + Send + Sync>);
        let first_done = tokio::sync::Notify::new();

        block_on(async {
            let refresh1 = async {
                c.refresh().await;
                first_done.notify_one();
            };
            let refresh2 = c.refresh();
            let orchestrate = async {
                repo.wait_for_call(0).await;
                repo.wait_for_call(1).await;
                repo.release_call(0);
                first_done.notified().await;
                assert!(!c.first_load_settled.get_untracked());
                repo.release_call(1);
            };
            tokio::join!(refresh1, refresh2, orchestrate);
        });

        assert!(c.first_load_settled.get_untracked());
    }
}
