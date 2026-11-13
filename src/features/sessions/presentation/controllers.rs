use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;

use clean_signals::{ControllerCore, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set, WithUntracked};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::{Notify, broadcast};

use crate::agents::agent_sort_key;
use crate::connection::{ConnectionController, ServerInfo};
use crate::failure::AppFailure;
use crate::features::connect::domain::entities::epoch_millis;
use crate::features::servers::di::{LastSeenHandle, use_last_seen};
use crate::features::servers::domain::entities::{PageSnapshot, SeenServer, SessionGlimpse};
use crate::features::sessions::domain::entities::{
    CreateSessionParams, RenameRequest, Session, SessionAgent, SessionBackend,
};
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::features::sessions::domain::use_cases::{
    CreateSession, KillSession, ListSessions, RenameSession,
};
use crate::net::wake::WakeReason;

/// Sentinel `SessionsController::resume_at` holds while polling runs; no real navigator depth reaches it.
const NOT_PAUSED: usize = usize::MAX;

pub const POLL_PERIOD_SECS: u32 = 3;

/// Consecutive `Network` failures of the poll and wake refreshes after which the screen reports
/// connection trouble; twice as many make it persistent.
pub const RENEW_AFTER_FAILURES: u32 = 2;

/// Whether `failure` is the connection's to show (the banner), not a toast's.
fn is_connection_class(failure: &AppFailure) -> bool {
    matches!(failure, AppFailure::Network(_) | AppFailure::Auth(_))
}

/// The failure streak's edge, reported once ([`SessionsController::take_trouble`]).
/// `Auth` never counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionTrouble {
    Started,
    Persistent,
    Recovered,
}

fn glimpse_of(session: &Session) -> SessionGlimpse {
    SessionGlimpse {
        name: session.name.clone(),
        backend: session.backend.label().to_string(),
        running: session.status.is_live(),
        meta_line: format!(
            "{} · {} tabs · {} panes",
            session.age_label(),
            session.layout.tabs,
            session.layout.panes
        ),
    }
}

/// Inert Sessions snapshots built in this process; a suite reads it to prove a closed stage builds none.
pub fn snapshots_built_for_tests() -> usize {
    super::widgets::snapshots_built()
}

/// Releases a single-flight claim when dropped, however the claimed work ends.
struct ReleaseClaim<'a>(&'a AtomicBool);

impl Drop for ReleaseClaim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub struct SessionsController {
    core: ControllerCore<AppFailure>,
    list: ListSessions,
    create: CreateSession,
    rename: RenameSession,
    kill: KillSession,
    pub sessions: RwSignal<Vec<Session>>,
    pub loaded: RwSignal<bool>,
    /// Signals when the first `refresh()` has settled, success or failure; releases the loading-rain overlay.
    pub first_load_settled: RwSignal<bool>,
    /// The Tablet detail's selection, keyed by [`Session::id`]; cleared by a load whose set lacks that id.
    pub selected: RwSignal<Option<String>>,
    pub agents: RwSignal<Vec<SessionAgent>>,
    repository: Arc<dyn SessionsRepository + Send + Sync>,
    glimpse_sink: Option<(LastSeenHandle, Arc<ConnectionController>)>,
    detail_visible: AtomicBool,
    agents_for: Mutex<Option<String>>,
    /// Bumped by every agents fetch and retarget: only the newest fetch for the current target publishes.
    agents_generation: AtomicU64,
    /// The poll tick's own single-flight claim on the agents fetch: a hung agents read never stalls the list.
    agents_in_flight: AtomicBool,
    /// A cached snapshot with no live connection behind it: no poll, wake refresh or retry runs.
    offline: AtomicBool,
    resume_at: AtomicUsize,
    polls: AtomicU32,
    poll_in_flight: AtomicBool,
    generation: AtomicU64,
    failure_streak: AtomicU32,
    trouble: Mutex<Option<ConnectionTrouble>>,
}

impl SessionsController {
    pub fn new(repository: Arc<dyn SessionsRepository + Send + Sync>) -> Self {
        Self {
            core: ControllerCore::new(),
            list: ListSessions::new(Arc::clone(&repository)),
            create: CreateSession::new(Arc::clone(&repository)),
            rename: RenameSession::new(Arc::clone(&repository)),
            kill: KillSession::new(Arc::clone(&repository)),
            sessions: RwSignal::new(Vec::new()),
            loaded: RwSignal::new(false),
            first_load_settled: RwSignal::new(false),
            selected: RwSignal::new(None),
            agents: RwSignal::new(Vec::new()),
            repository,
            glimpse_sink: use_last_seen().zip(frust::use_context::<Arc<ConnectionController>>()),
            detail_visible: AtomicBool::new(false),
            agents_for: Mutex::new(None),
            agents_generation: AtomicU64::new(0),
            agents_in_flight: AtomicBool::new(false),
            offline: AtomicBool::new(false),
            resume_at: AtomicUsize::new(NOT_PAUSED),
            polls: AtomicU32::new(0),
            poll_in_flight: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            failure_streak: AtomicU32::new(0),
            trouble: Mutex::new(None),
        }
    }

    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    pub async fn refresh(&self) {
        self.refresh_with(false).await;
    }

    /// A refresh nobody asked for (first load, poll, wake, restart): connection-class failures
    /// (Network, Auth, not connected) are the banner's and raise no toast.
    pub async fn refresh_background(&self) {
        self.refresh_with(true).await;
    }

    /// `Some(outcome)` once the reply is current; `None` when a newer refresh superseded it.
    async fn refresh_with(&self, background: bool) -> Option<Result<(), AppFailure>> {
        let my_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let source = self.glimpse_source();
        let result = self
            .core
            .run(
                &self.list,
                NoParams,
                RunOptions {
                    emit_failure: !background,
                    ..RunOptions::default()
                },
            )
            .await;
        if self.generation.load(Ordering::SeqCst) != my_generation {
            return None;
        }
        if let Err(failure) = &result
            && background
            && !is_connection_class(failure)
            && !self.core.is_disposed()
        {
            self.core.failures().emit(failure);
        }
        let outcome = match result {
            Ok(sessions) => {
                self.forget_vanished_selection(&sessions);
                self.publish_glimpse(source, &sessions);
                self.sessions.set(sessions);
                self.loaded.set(true);
                self.retarget_agents();
                Ok(())
            }
            Err(failure) => Err(failure),
        };
        self.first_load_settled.set(true);
        Some(outcome)
    }

    fn publish_glimpse(&self, source: Option<ServerInfo>, sessions: &[Session]) {
        let (Some((handle, connection)), Some(info)) = (&self.glimpse_sink, source) else {
            return;
        };
        let endpoint = info.endpoint.clone();
        let page = PageSnapshot {
            sessions: sessions.to_vec(),
            read_only: connection.is_read_only.get_untracked(),
            info: Some(info),
        };
        handle.record_seen(
            &endpoint,
            SeenServer {
                glimpses: sessions.iter().map(glimpse_of).collect(),
                seen_at_ms: epoch_millis(),
                page: Some(Arc::new(page)),
            },
        );
    }

    /// Captured before the list awaits: a switch landing mid-list must not file the old server's
    /// rows under the new endpoint.
    fn glimpse_source(&self) -> Option<ServerInfo> {
        let (_, connection) = self.glimpse_sink.as_ref()?;
        connection.server_info.get_untracked()
    }

    /// Replaces the list with a cached snapshot without ever emptying it; a reply in flight from the
    /// previous server is dropped and a selection the snapshot lacks is forgotten.
    pub fn seed_from(&self, page: &PageSnapshot) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.reset_streak();
        self.poll_in_flight.store(false, Ordering::SeqCst);
        self.agents_in_flight.store(false, Ordering::SeqCst);
        self.forget_vanished_selection(&page.sessions);
        self.sessions.set(page.sessions.clone());
        self.loaded.set(true);
        self.first_load_settled.set(true);
        self.retarget_agents();
    }

    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::Relaxed);
    }

    pub fn is_offline(&self) -> bool {
        self.offline.load(Ordering::Relaxed)
    }

    pub fn refresh_in_place(self: &Arc<Self>) {
        self.set_offline(false);
        let controller = Arc::clone(self);
        frust::spawn_local(async move { controller.refresh_background().await });
    }

    /// Starts this mount over for a new connection: the generation bump drops any in-flight reply,
    /// the old list, selection and agents go, and the first-load flags reset so the loading rain replays.
    pub fn restart(self: &Arc<Self>) {
        self.set_offline(false);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.reset_streak();
        self.sessions.set(Vec::new());
        self.loaded.set(false);
        self.first_load_settled.set(false);
        self.selected.set(None);
        self.poll_in_flight.store(false, Ordering::SeqCst);
        self.agents_in_flight.store(false, Ordering::SeqCst);
        self.retarget_agents();
        let controller = Arc::clone(self);
        frust::spawn_local(async move { controller.refresh_background().await });
    }

    pub async fn refresh_from_poll(&self) {
        {
            let _release = ReleaseClaim(&self.poll_in_flight);
            let outcome = self.refresh_with(true).await;
            self.count_outcome(outcome);
        }
        // Past the list poll's claim: the next tick's list refresh does not wait on this read.
        self.refresh_agents_from_poll().await;
    }

    fn reset_streak(&self) {
        self.failure_streak.store(0, Ordering::SeqCst);
        *self.trouble_slot() = None;
    }

    fn trouble_slot(&self) -> std::sync::MutexGuard<'_, Option<ConnectionTrouble>> {
        self.trouble.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Folds one poll or wake refresh into the failure streak. Only `Network` failures count; a
    /// success resets it, raising `Recovered` if trouble was raised.
    fn count_outcome(&self, outcome: Option<Result<(), AppFailure>>) {
        let edge = match outcome {
            Some(Err(AppFailure::Network(_))) => {
                match self.failure_streak.fetch_add(1, Ordering::SeqCst) + 1 {
                    n if n == RENEW_AFTER_FAILURES => Some(ConnectionTrouble::Started),
                    n if n == 2 * RENEW_AFTER_FAILURES => Some(ConnectionTrouble::Persistent),
                    _ => None,
                }
            }
            Some(Ok(())) => (self.failure_streak.swap(0, Ordering::SeqCst) >= RENEW_AFTER_FAILURES)
                .then_some(ConnectionTrouble::Recovered),
            Some(Err(_)) | None => None,
        };
        if edge.is_some() {
            *self.trouble_slot() = edge;
        }
    }

    pub fn take_trouble(&self) -> Option<ConnectionTrouble> {
        self.trouble_slot().take()
    }

    fn is_connected(&self) -> bool {
        self.glimpse_sink
            .as_ref()
            .is_none_or(|(_, connection)| connection.connected.get_untracked())
    }

    /// A tick finding the previous tick's agents read still outstanding skips it.
    async fn refresh_agents_from_poll(&self) {
        if self.agents_in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        let _release = ReleaseClaim(&self.agents_in_flight);
        self.refresh_agents().await;
    }

    /// On a hidden-to-visible edge with a selected live session the agents are fetched at once; on
    /// visible-to-hidden the list is cleared. A repeat of the same value is a no-op.
    pub fn set_detail_visible(self: &Arc<Self>, visible: bool) {
        let was = self.detail_visible.swap(visible, Ordering::Relaxed);
        if was == visible {
            return;
        }
        if self.retarget_agents().is_some() {
            let controller = Arc::clone(self);
            frust::spawn_local(async move { controller.refresh_agents().await });
        }
    }

    fn agents_target(&self) -> Option<String> {
        if !self.detail_visible.load(Ordering::Relaxed) {
            return None;
        }
        let selected = self.selected.try_get_untracked().flatten()?;
        let wanted = self.sessions.try_with_untracked(|sessions| {
            sessions.iter().any(|session| {
                session.id == selected
                    && session.status.is_live()
                    && session.agents.as_ref().is_some_and(|a| !a.is_empty())
            })
        })?;
        wanted.then_some(selected)
    }

    fn retarget_agents(&self) -> Option<String> {
        let target = self.agents_target();
        let mut current = self
            .agents_for
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *current != target {
            *current = target.clone();
            drop(current);
            self.agents_generation.fetch_add(1, Ordering::SeqCst);
            self.publish_agents(Vec::new());
        }
        target
    }

    /// Generation-guarded (an older fetch never overwrites a newer one) and never written after
    /// dispose; a failure keeps the last list for the next tick to retry.
    pub async fn refresh_agents(&self) {
        let Some(target) = self.retarget_agents() else {
            return;
        };
        let generation = self.agents_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let answer = self.repository.list_agents(target).await;
        if self.agents_generation.load(Ordering::SeqCst) != generation || !self.alive() {
            return;
        }
        match answer {
            Ok(mut agents) => {
                agents.sort_by_key(|agent| agent_sort_key(agent.status, agent.change_seq));
                self.publish_agents(agents);
            }
            Err(failure) => {
                log::debug!("sessions: list_agents failed, keeping the rows: {failure}")
            }
        }
    }

    fn alive(&self) -> bool {
        !self.core.is_disposed() && self.agents.try_with_untracked(|_| ()).is_some()
    }

    /// Read-compare-then-write, so an unchanged list never rebuilds.
    fn publish_agents(&self, agents: Vec<SessionAgent>) {
        if self.agents.try_with_untracked(|current| *current != agents) == Some(true) {
            self.agents.set(agents);
        }
    }

    /// The wake reaction: one refresh through the poll's single-flight gate, so a wake during a poll
    /// starts nothing. Returns whether it started one.
    ///
    /// A wake on a first load that settled failed (no poll runs) re-runs that load instead.
    pub async fn refresh_on_wake(&self) -> bool {
        if !self.take_poll_tick() && !self.take_retry() {
            return false;
        }
        self.refresh_from_poll().await;
        true
    }

    fn take_retry(&self) -> bool {
        self.is_polling()
            && !self.is_offline()
            && self.is_connected()
            && self.first_load_settled.get_untracked()
            && !self.loaded.get_untracked()
            && !self.poll_in_flight.swap(true, Ordering::SeqCst)
    }

    pub async fn retry_first_load(&self) {
        if !self.loaded.get_untracked() {
            self.refresh_background().await;
        }
    }

    /// **Unchecked**: an id outside the loaded set is stored as given and the next refresh forgets it.
    pub fn select(&self, id: &str) {
        if self.selected.get_untracked().as_deref() != Some(id) {
            self.selected.set(Some(id.to_string()));
        }
        self.retarget_agents();
    }

    pub fn select_and_refresh_agents(self: &Arc<Self>, id: &str) {
        self.select(id);
        let controller = Arc::clone(self);
        frust::spawn_local(async move { controller.refresh_agents().await });
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

    pub async fn create(&self, name: String, backend: SessionBackend) {
        let params = CreateSessionParams { name, backend };
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
        let was_selected = self.selected.get_untracked().as_deref() == Some(id.as_str());
        let old_id = id.clone();
        let renamed = self
            .core
            .run(
                &self.rename,
                RenameRequest { id, name },
                RunOptions::default(),
            )
            .await;
        if let Ok(renamed) = renamed {
            // The repository mints the id from the name, so a rename moves it: follow it while the selection
            // still holds the old id (or a refresh cleared it); one the user moved elsewhere stays put.
            let current = self.selected.get_untracked();
            if was_selected && (current.is_none() || current.as_deref() == Some(old_id.as_str())) {
                self.selected.set(Some(renamed.id));
            }
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

    pub fn session(&self, id: &str) -> Option<Session> {
        self.sessions
            .get_untracked()
            .into_iter()
            .find(|s| s.id == id)
    }

    pub fn take_poll_tick(&self) -> bool {
        let poll = self.is_polling()
            && !self.is_offline()
            && self.is_connected()
            && self.loaded.get_untracked()
            && !self.poll_in_flight.swap(true, Ordering::SeqCst);
        if poll {
            self.polls.fetch_add(1, Ordering::Relaxed);
        }
        poll
    }

    pub fn is_polling(&self) -> bool {
        self.resume_at.load(Ordering::Relaxed) == NOT_PAUSED
    }

    pub fn polls_taken(&self) -> u32 {
        self.polls.load(Ordering::Relaxed)
    }

    /// Pause the poll until the navigator stack is back at `depth`. Repeated calls keep the
    /// shallowest mark.
    pub fn pause_until_depth(&self, depth: usize) {
        self.resume_at.fetch_min(depth, Ordering::Relaxed);
    }

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

/// Calls `on_wake` once per burst of wakes on `wakes`, until `cancel` is notified or the bus
/// closes. The burst reports its strongest reason: [`WakeReason::NetworkChanged`] if any wake was
/// one. A lagged receiver counts as a wake, never an error, and as a network change (errs towards
/// a redial).
///
/// `cancel` is raced against the wait itself, so a stopped screen's loop ends at once; `Notify`
/// keeps a permit, so a cancel between waits is not lost.
pub async fn each_wake(
    mut wakes: broadcast::Receiver<WakeReason>,
    cancel: &Notify,
    mut on_wake: impl FnMut(WakeReason),
) {
    loop {
        let mut reason = match recv_or_cancel(&mut wakes, cancel).await {
            None | Some(Err(broadcast::error::RecvError::Closed)) => break,
            Some(Ok(reason)) => reason,
            Some(Err(broadcast::error::RecvError::Lagged(_))) => WakeReason::NetworkChanged,
        };
        loop {
            match wakes.try_recv() {
                Ok(WakeReason::NetworkChanged) | Err(TryRecvError::Lagged(_)) => {
                    reason = WakeReason::NetworkChanged;
                }
                Ok(WakeReason::Foregrounded) => {}
                Err(_) => break,
            }
        }
        on_wake(reason);
    }
}

/// One wait on `wakes`, or `None` the moment `cancel` fires; a pending cancel beats a ready wake.
async fn recv_or_cancel(
    wakes: &mut broadcast::Receiver<WakeReason>,
    cancel: &Notify,
) -> Option<Result<WakeReason, broadcast::error::RecvError>> {
    let mut cancelled = std::pin::pin!(cancel.notified());
    let mut recv = std::pin::pin!(wakes.recv());
    std::future::poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        recv.as_mut().poll(cx).map(Some)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::sessions::data::repositories::MockSessionsRepository;
    use crate::features::sessions::data::repositories::mock_sessions_repository::SEEDED_DEV_ID;
    use crate::features::sessions::domain::entities::{
        CreateSessionParams, LayoutSummary, SessionStatus, ThirdMetric,
    };

    const SEEDED_COUNT: usize = 3;

    /// Test wiring: the mock repository behind the domain trait object; `di.rs` stays the only
    /// production site where presentation meets `data`.
    fn controller() -> SessionsController {
        SessionsController::new(Arc::new(MockSessionsRepository::seeded()))
    }

    /// Drive a controller call to completion on a current-thread runtime, with no frust executor.
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
    fn publish_glimpse_fills_the_last_seen_handle_after_a_successful_list() {
        use crate::connection::{ConnectTarget, MockConnectionBackend, TrustMode};

        let connection = Arc::new(ConnectionController::with_backend(Arc::new(
            MockConnectionBackend,
        )));
        connection.connect(ConnectTarget {
            host: "rig".to_string(),
            port: 7777,
            trust_mode: TrustMode::NoVerification,
            label: "rig".to_string(),
            offer_credential: false,
            token: zeroize::Zeroizing::new("t".to_string()),
            remember: false,
            fingerprint: None,
        });
        let handle = LastSeenHandle::new();
        let mut c = controller();
        c.glimpse_sink = Some((handle.clone(), connection));

        block_on(c.refresh());

        let seen = handle.snapshot();
        let filed = seen.get("rig:7777").expect("filed under the endpoint");
        let glimpses = &filed.glimpses;
        assert_eq!(glimpses.len(), SEEDED_COUNT);
        assert!(filed.seen_at_ms > 0);
        let page = filed.page.as_ref().expect("the full page state is filed");
        assert_eq!(page.sessions, c.sessions.get_untracked());
        assert_eq!(
            page.info.as_ref().map(|info| info.endpoint.as_str()),
            Some("rig:7777")
        );
        assert!(
            glimpses
                .iter()
                .all(|g| !g.name.is_empty() && !g.meta_line.is_empty())
        );
    }

    #[test]
    fn publish_glimpse_files_nothing_without_a_handle_or_without_a_connected_endpoint() {
        use crate::connection::MockConnectionBackend;

        let c = controller();
        assert!(
            c.glimpse_sink.is_none(),
            "no handle provided in a bare suite"
        );
        block_on(c.refresh());
        assert_eq!(c.sessions.get_untracked().len(), SEEDED_COUNT);

        let handle = LastSeenHandle::new();
        let mut c = controller();
        c.glimpse_sink = Some((
            handle.clone(),
            Arc::new(ConnectionController::with_backend(Arc::new(
                MockConnectionBackend,
            ))),
        ));
        block_on(c.refresh());
        assert_eq!(c.sessions.get_untracked().len(), SEEDED_COUNT);
        assert!(
            handle.snapshot().is_empty(),
            "a handle with no connected endpoint to file under stays empty"
        );
    }

    #[test]
    fn create_round_trips_through_the_repository() {
        let c = controller();
        block_on(c.refresh());

        block_on(c.create("staging".to_string(), SessionBackend::Zellij));

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
        assert!(c.sessions.get_untracked().iter().any(|s| s.name == "dev-2"));
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

        c.pause_until_depth(1);
        assert!(!c.is_polling(), "the poll pauses for the pushed page");
        assert!(!c.take_poll_tick());
        assert_eq!(c.polls_taken(), 0, "a skipped tick is not counted");

        c.observe_depth(2);
        assert!(!c.is_polling());

        c.observe_depth(1);
        assert!(c.is_polling());
        assert!(c.take_poll_tick());
    }

    #[test]
    fn nested_pushes_keep_the_shallowest_resume_mark() {
        let c = controller();
        block_on(c.refresh());

        c.pause_until_depth(1);
        c.pause_until_depth(2);
        c.observe_depth(2);
        assert!(!c.is_polling(), "still covered by the first push");

        c.observe_depth(1);
        assert!(c.is_polling());
    }

    #[test]
    fn a_selection_follows_a_rename_that_changes_the_id() {
        let c = controller();
        block_on(c.refresh());
        let id = c.sessions.get_untracked()[0].id.clone();
        c.select(&id);

        block_on(c.rename(id.clone(), "dev-2".to_string()));
        let renamed = c.selected.get_untracked().expect("still selected");
        assert_ne!(renamed, id, "the mock mints the id from the new name");
        assert_eq!(
            c.session(&renamed).map(|s| s.name),
            Some("dev-2".to_string())
        );

        block_on(c.refresh());
        assert_eq!(c.selected.get_untracked(), Some(renamed));
    }

    #[test]
    fn a_selection_made_during_a_rename_is_not_pulled_back() {
        let c = controller();
        block_on(c.refresh());
        let sessions = c.sessions.get_untracked();
        let (a, b) = (sessions[0].id.clone(), sessions[1].id.clone());
        c.select(&a);

        block_on(async {
            tokio::join!(c.rename(a.clone(), "dev-2".to_string()), async {
                c.select(&b);
            });
        });

        assert_eq!(c.selected.get_untracked(), Some(b));
    }

    #[test]
    fn renaming_another_session_leaves_the_selection_alone() {
        let c = controller();
        block_on(c.refresh());
        let sessions = c.sessions.get_untracked();
        c.select(&sessions[0].id);

        block_on(c.rename(sessions[1].id.clone(), "other".to_string()));

        assert_eq!(c.selected.get_untracked(), Some(sessions[0].id.clone()));
    }

    #[test]
    fn a_selection_survives_a_failed_refresh() {
        let c = SessionsController::new(Arc::new(FailOnceSessionsRepository::default()));
        c.select(SEEDED_DEV_ID);

        block_on(c.refresh());
        assert!(!c.loaded.get_untracked(), "the first load failed");
        assert_eq!(c.selected.get_untracked().as_deref(), Some(SEEDED_DEV_ID));

        block_on(c.refresh());
        assert_eq!(c.selected.get_untracked().as_deref(), Some(SEEDED_DEV_ID));
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

    fn local_block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime");
        tokio::task::LocalSet::new().block_on(&rt, fut)
    }

    #[test]
    fn a_wake_refreshes_the_list_before_the_next_poll_tick() {
        use crate::net::wake::WakeBus;

        let repo = Arc::new(GatedSessionsRepository::new(0));
        let c = Arc::new(SessionsController::new(
            Arc::clone(&repo) as Arc<dyn SessionsRepository + Send + Sync>
        ));

        local_block_on(async {
            c.refresh().await;
            assert_eq!(repo.calls.load(Ordering::SeqCst), 1);

            let bus = WakeBus::new();
            let wakes = bus.subscribe();
            bus.publish(WakeReason::NetworkChanged);
            drop(bus);

            let started = std::cell::RefCell::new(Vec::new());
            each_wake(wakes, &Notify::new(), |_| {
                let c = Arc::clone(&c);
                started
                    .borrow_mut()
                    .push(tokio::task::spawn_local(async move {
                        c.refresh_on_wake().await
                    }));
            })
            .await;
            for handle in started.into_inner() {
                assert!(handle.await.expect("the refresh task finishes"));
            }

            assert_eq!(
                repo.calls.load(Ordering::SeqCst),
                2,
                "one wake, one refresh"
            );
            assert_eq!(c.sessions.get_untracked(), vec![gated_session(1)]);
            assert!(c.take_poll_tick(), "the wake left the poll gate free");
        });
    }

    #[test]
    fn a_wake_during_an_in_flight_refresh_starts_no_second_one() {
        use crate::net::wake::WakeBus;

        let repo = Arc::new(GatedSessionsRepository::new(2));
        // The first load goes straight through; the poll's refresh parks on call 1.
        repo.release_call(0);
        let c = Arc::new(SessionsController::new(
            Arc::clone(&repo) as Arc<dyn SessionsRepository + Send + Sync>
        ));

        local_block_on(async {
            c.refresh().await;

            assert!(c.take_poll_tick(), "a poll tick claims the gate");
            let poll = tokio::task::spawn_local({
                let c = Arc::clone(&c);
                async move { c.refresh_from_poll().await }
            });
            repo.wait_for_call(1).await;

            let bus = WakeBus::new();
            let wakes = bus.subscribe();
            bus.publish(WakeReason::Foregrounded);
            drop(bus);

            let started = std::cell::RefCell::new(Vec::new());
            each_wake(wakes, &Notify::new(), |_| {
                let c = Arc::clone(&c);
                started
                    .borrow_mut()
                    .push(tokio::task::spawn_local(async move {
                        c.refresh_on_wake().await
                    }));
            })
            .await;
            for handle in started.into_inner() {
                assert!(
                    !handle.await.expect("the wake task finishes"),
                    "the in-flight poll holds the gate, so the wake starts nothing"
                );
            }
            assert_eq!(repo.calls.load(Ordering::SeqCst), 2);

            repo.release_call(1);
            poll.await.expect("the poll finishes");
            assert_eq!(repo.calls.load(Ordering::SeqCst), 2, "no second refresh");
        });
    }

    #[test]
    fn a_lagged_wake_stream_is_one_wake() {
        use crate::net::wake::WakeBus;

        let bus = WakeBus::new();
        let wakes = bus.subscribe();
        // Far past the buffer: the receiver lags before it ever polls.
        for _ in 0..64 {
            bus.publish(WakeReason::NetworkChanged);
        }
        drop(bus);

        let mut calls = 0;
        block_on(each_wake(wakes, &Notify::new(), |_| calls += 1));

        assert_eq!(calls, 1, "a lag is one wake, never an error");
    }

    #[test]
    fn no_wake_means_no_refresh() {
        use crate::net::wake::WakeBus;

        let repo = Arc::new(GatedSessionsRepository::new(0));
        let c =
            SessionsController::new(Arc::clone(&repo) as Arc<dyn SessionsRepository + Send + Sync>);
        block_on(c.refresh());

        let bus = WakeBus::new();
        let wakes = bus.subscribe();
        drop(bus);
        let mut calls = 0;
        block_on(each_wake(wakes, &Notify::new(), |_| calls += 1));

        assert_eq!(calls, 0);
        assert_eq!(repo.calls.load(Ordering::SeqCst), 1, "only the first load");
    }

    #[test]
    fn a_wake_before_the_first_load_settles_starts_nothing() {
        let c = controller();

        assert!(!block_on(c.refresh_on_wake()));
        assert!(!c.loaded.get_untracked());
    }

    #[test]
    fn a_stopped_screen_ends_its_wake_loop_without_waiting_for_a_wake() {
        use crate::net::wake::WakeBus;

        // The bus stays open and nothing is published: only the cancel can end the loop.
        let bus = WakeBus::new();
        let wakes = bus.subscribe();
        let cancel = Notify::new();
        cancel.notify_one();

        let mut calls = 0;
        block_on(each_wake(wakes, &cancel, |_| calls += 1));

        assert_eq!(calls, 0, "an unmounted screen reacts to nothing");
        drop(bus);
    }

    #[test]
    fn a_cancel_ends_a_loop_that_is_parked_waiting() {
        use crate::net::wake::WakeBus;

        let bus = WakeBus::new();
        let wakes = bus.subscribe();
        let cancel = Notify::new();

        local_block_on(async {
            let mut calls = 0;
            let waiting = each_wake(wakes, &cancel, |_| calls += 1);
            let stop = async {
                tokio::task::yield_now().await;
                cancel.notify_one();
            };
            tokio::join!(waiting, stop);
            assert_eq!(calls, 0);
        });
        drop(bus);
    }

    #[test]
    fn a_burst_reports_the_strongest_reason() {
        use crate::net::wake::WakeBus;

        let bus = WakeBus::new();
        let wakes = bus.subscribe();
        bus.publish(WakeReason::Foregrounded);
        bus.publish(WakeReason::NetworkChanged);
        bus.publish(WakeReason::Foregrounded);
        let calm = bus.subscribe();
        bus.publish(WakeReason::Foregrounded);
        bus.publish(WakeReason::Foregrounded);
        drop(bus);

        let mut reasons = Vec::new();
        block_on(each_wake(wakes, &Notify::new(), |r| reasons.push(r)));
        assert_eq!(reasons, vec![WakeReason::NetworkChanged]);

        let mut reasons = Vec::new();
        block_on(each_wake(calm, &Notify::new(), |r| reasons.push(r)));
        assert_eq!(reasons, vec![WakeReason::Foregrounded]);
    }

    #[test]
    fn a_wake_on_a_failed_first_load_retries_it() {
        let repo = Arc::new(FailOnceSessionsRepository::default());
        let c =
            SessionsController::new(Arc::clone(&repo) as Arc<dyn SessionsRepository + Send + Sync>);
        block_on(c.refresh());
        assert!(c.first_load_settled.get_untracked());
        assert!(!c.loaded.get_untracked(), "the first load failed");
        assert_eq!(repo.calls.load(Ordering::SeqCst), 1);

        assert!(block_on(c.refresh_on_wake()), "the wake re-ran the load");

        assert_eq!(
            repo.calls.load(Ordering::SeqCst),
            2,
            "a second ListSessions"
        );
        assert!(c.loaded.get_untracked());
        assert!(c.take_poll_tick(), "the poll is re-armed");
    }

    #[test]
    fn a_wake_over_a_covered_screen_does_not_retry_a_failed_first_load() {
        let c = SessionsController::new(Arc::new(FailOnceSessionsRepository::default()));
        block_on(c.refresh());
        c.pause_until_depth(0);

        assert!(!block_on(c.refresh_on_wake()));
        assert!(!c.loaded.get_untracked());
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
            agents: None,
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
