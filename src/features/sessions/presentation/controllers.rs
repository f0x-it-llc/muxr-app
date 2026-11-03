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

/// The sentinel [`SessionsController::resume_at`] holds while polling is
/// running — no real navigator depth ever reaches it.
const NOT_PAUSED: usize = usize::MAX;

pub const POLL_PERIOD_SECS: u32 = 3;

/// One session as a non-live switcher face remembers it.
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

/// How many inert Sessions snapshots (the switcher's cached peer cards) this
/// process has built: what a suite reads to prove a closed stage builds none.
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
    /// Signals when the first `refresh()` has settled (completed, whether
    /// success or failure), releasing the overlay that holds the loading rain.
    pub first_load_settled: RwSignal<bool>,
    /// The Tablet detail's selection, keyed by [`Session::id`] — cleared by a
    /// load whose set no longer carries that id.
    pub selected: RwSignal<Option<String>>,
    /// The Tablet detail's agent list: the selected session's agents, most
    /// urgent first. Fetched by [`refresh_agents`](Self::refresh_agents) on a
    /// selection change and every poll tick while the detail shows them.
    pub agents: RwSignal<Vec<SessionAgent>>,
    repository: Arc<dyn SessionsRepository + Send + Sync>,
    /// Where a successful list is remembered for the server switcher's
    /// non-live faces: the app's last-seen handle and the connection naming
    /// the endpoint it belongs to. Both resolved once, here; `None` (no
    /// handle provided, as in every headless suite) makes
    /// [`publish_glimpse`](Self::publish_glimpse) a no-op.
    glimpse_sink: Option<(LastSeenHandle, Arc<ConnectionController>)>,
    /// Whether the detail pane is on screen — the page sets it from its size
    /// class each build, so a Compact window never pays for the agent reads.
    detail_visible: AtomicBool,
    /// The session [`agents`](Self::agents) belongs to, if any.
    agents_for: Mutex<Option<String>>,
    /// Bumped by every agents fetch and every retarget: only the newest fetch
    /// for the current target publishes.
    agents_generation: AtomicU64,
    /// The poll tick's own single-flight claim on the agents fetch, apart from
    /// [`poll_in_flight`](Self::poll_in_flight): a hung agents read never stalls the list.
    agents_in_flight: AtomicBool,
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
        // Named before the await (see `glimpse_source`).
        let source = self.glimpse_source();
        let result = self
            .core
            .run(&self.list, NoParams, RunOptions::default())
            .await;
        if self.generation.load(Ordering::SeqCst) != my_generation {
            return;
        }
        if let Ok(sessions) = result {
            self.forget_vanished_selection(&sessions);
            self.publish_glimpse(source, &sessions);
            self.sessions.set(sessions);
            self.loaded.set(true);
            self.retarget_agents();
        }
        self.first_load_settled.set(true);
    }

    /// Remember `sessions` as what the connected server last showed, for the
    /// server switcher's non-live faces: the glimpse list and the whole page
    /// state (list items as rendered plus the header facts) its snapshot card
    /// re-renders. Plain data through the servers slice's last-seen handle;
    /// does nothing without one, or with no connected server to file it under.
    /// `source` is the handshake facts captured before the list awaited.
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

    /// The connected server's handshake facts, when a glimpse sink exists to
    /// file them. Named before the list's await: a switch landing mid-list
    /// must not file the old server's rows under the new endpoint.
    fn glimpse_source(&self) -> Option<ServerInfo> {
        let (_, connection) = self.glimpse_sink.as_ref()?;
        connection.server_info.get_untracked()
    }

    /// Starts this mount over for a new connection: the generation bump drops
    /// any in-flight refresh's reply (a stale poll, wake or mutation refresh
    /// from the old server lands nowhere), the old server's list, selection and
    /// agents go, the first-load flags reset so the loading rain can replay, the
    /// poll gate is released, and the first load is kicked as `init` does.
    pub fn restart(self: &Arc<Self>) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.sessions.set(Vec::new());
        self.loaded.set(false);
        self.first_load_settled.set(false);
        self.selected.set(None);
        self.poll_in_flight.store(false, Ordering::SeqCst);
        self.agents_in_flight.store(false, Ordering::SeqCst);
        self.retarget_agents();
        let controller = Arc::clone(self);
        frust::spawn_local(async move { controller.refresh().await });
    }

    pub async fn refresh_from_poll(&self) {
        {
            let _release = ReleaseClaim(&self.poll_in_flight);
            self.refresh().await;
        }
        // Past the list poll's claim: the next tick's list refresh does not wait on this read.
        self.refresh_agents_from_poll().await;
    }

    /// The poll tick's agent fetch under its own single-flight claim: a tick finding the
    /// previous tick's read still outstanding skips it.
    async fn refresh_agents_from_poll(&self) {
        if self.agents_in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        let _release = ReleaseClaim(&self.agents_in_flight);
        self.refresh_agents().await;
    }

    /// Record whether the detail pane is on screen — the page's size class,
    /// forwarded each build. On a hidden-to-visible edge with a selected live session
    /// the controller fetches its agents at once (its own task, so the page does no I/O);
    /// on a visible-to-hidden edge the list is cleared. A repeat of the same value is a
    /// plain no-op.
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

    /// The session whose agents the detail shows now: the selected one, while
    /// the detail is visible, the session is live and its roll-up names agents.
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

    /// Point [`agents`](Self::agents) at [`agents_target`](Self::agents_target):
    /// on a change the list empties at once and any fetch in flight for the old
    /// target lands nowhere. Answers the current target.
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

    /// Fetch the detail's agents for the current target — on a selection change
    /// and on every poll tick, so a status swap that leaves the roll-up's counts
    /// alone still reaches the rows. Generation-guarded (an older fetch never
    /// overwrites a newer one), never written after dispose, and a failure keeps
    /// the last list for the next tick to retry.
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

    /// Whether the hosted state is still mounted — `false` once disposed.
    fn alive(&self) -> bool {
        !self.core.is_disposed() && self.agents.try_with_untracked(|_| ()).is_some()
    }

    /// Read-compare-then-write, so an unchanged list never rebuilds.
    fn publish_agents(&self, agents: Vec<SessionAgent>) {
        if self.agents.try_with_untracked(|current| *current != agents) == Some(true) {
            self.agents.set(agents);
        }
    }

    /// The wake reaction: one refresh through the poll's own single-flight
    /// gate ([`take_poll_tick`](Self::take_poll_tick)), so a wake that lands
    /// while a poll (or an earlier wake) is in flight starts nothing. Returns
    /// whether it started a refresh.
    ///
    /// A wake on a first load that settled *failed* (the Retry state, where
    /// no poll runs) re-runs that load instead, through the same claim; a
    /// wake before the first load settles still starts nothing.
    pub async fn refresh_on_wake(&self) -> bool {
        if !self.take_poll_tick() && !self.take_retry() {
            return false;
        }
        self.refresh_from_poll().await;
        true
    }

    /// Claims the poll gate for a re-run of a settled-but-failed first load.
    /// Honours the same covered-screen pause as
    /// [`take_poll_tick`](Self::take_poll_tick).
    fn take_retry(&self) -> bool {
        self.is_polling()
            && self.first_load_settled.get_untracked()
            && !self.loaded.get_untracked()
            && !self.poll_in_flight.swap(true, Ordering::SeqCst)
    }

    /// Re-runs a failed first load; its success sets `loaded`, the gate
    /// [`take_poll_tick`](Self::take_poll_tick) reads, so the poll re-arms.
    pub async fn retry_first_load(&self) {
        if !self.loaded.get_untracked() {
            self.refresh().await;
        }
    }

    /// Points the Tablet detail at `id`. **Unchecked**: an id that is not in
    /// the loaded set is stored as given (the detail pane tolerates an unknown
    /// id and shows its empty state), and the next refresh forgets it.
    pub fn select(&self, id: &str) {
        if self.selected.get_untracked().as_deref() != Some(id) {
            self.selected.set(Some(id.to_string()));
        }
        self.retarget_agents();
    }

    /// A Tablet card tap: [`select`](Self::select), then fetch the detail's agents
    /// without waiting for the next poll tick. The fetch is the controller's own
    /// task, so the page only forwards the tap.
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
            // The repository mints the id from the name (`<backend>:<name>`),
            // so a rename moves it: follow it, or the refresh would find the
            // old id gone and clear the selection. Only while the selection
            // still holds the old id (or a refresh in flight cleared it as
            // vanished): one the user moved elsewhere stays put.
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

/// Calls `on_wake` once per burst of wakes on `wakes`, until `cancel` is
/// notified or the bus closes. The burst reports its strongest reason:
/// [`WakeReason::NetworkChanged`] if any wake in it was one. A lagged receiver
/// means wakes were dropped, which still means "a wake happened": it is
/// treated as a wake, never an error, and (its reasons being lost) as a
/// network change, the side that errs towards a redial. The already-queued
/// wakes are coalesced into the same single call.
///
/// `cancel` is raced against the wait itself, so a stopped screen's loop ends
/// at once rather than at its next wake. `Notify` keeps a permit when nobody
/// is waiting, so a cancel that lands between waits is not lost.
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

/// One wait on `wakes`, or `None` the moment `cancel` fires. `cancel` is
/// polled first, so a pending cancel beats a ready wake.
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

    /// [`block_on`] under a `LocalSet`, so a test can `spawn_local` the
    /// refreshes a wake starts and await them.
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
        // The first load goes straight through; the poll's refresh parks on
        // call 1.
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

        // The bus stays open and nothing is published: only the cancel can
        // end the loop. (Retargeted from "on the next wake": the loop used to
        // outlive its screen until one arrived.)
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
