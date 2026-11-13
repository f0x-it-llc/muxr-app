//! [`GrpcTerminalRepository`] — the real, `muxrd`-backed [`TerminalRepository`], and
//! `terminal::di::provide`'s production choice.
//!
//! Every call resolves [`ConnectionController::client`] fresh — the sanctioned path to the live
//! [`MuxrClient`] — rather than dialing a channel of its own; `None` (disconnected) maps to
//! [`AppFailure::Network`] rather than panicking. The `Arc<ConnectionController>` field is this
//! feature's one `data` → `crate::connection` edge, resolved once in `di::provide`.
//!
//! **What it addresses, and how it learns it.** The read/detach methods take no session id, so
//! this repository is [`bind`](TerminalRepository::bind)-ed: the screen hands it the attached
//! session on mount and the connection id the `AttachTerminal` stream minted as soon as one
//! exists (and again on every re-attach). Both ride on every request, which is what makes a
//! space/tab switch land on this client's view; an unbound session is not an error, and a bare
//! `/terminal` answers empty. Two calls never reach the wire: `tab_lines` (on a live session a
//! tab's content is the emulator's) and `detach_session` (detaching *is* dropping the stream,
//! which `GrpcTerminalFeed`'s own [`Drop`] does on unmount).
//!
//! **Tab keys are derived, not carried**, and uniqueness is load-bearing —
//! [`map_tabs`] and [`crate::net::agent_layouts::unique_tab_key`] state that contract. Failure mapping funnels through
//! [`crate::net::error::from_status`], with `UNAUTHENTICATED` additionally invoking
//! [`ConnectionController::on_session_expired_for`] with the client the call used; every
//! `ActionAck`-returning call maps through [`crate::net::error::ack_result`] rather than
//! trusting the outer `Status`.
//!
//! **The agents stream.** `watch_agents` runs `WatchAgents` on a background [`frust::spawn`]
//! task that writes no signal: events reach the caller's sink, a transport failure reconnects on
//! the terminal feed's [`RECONNECT_BACKOFF`] (reset by a stream that delivered), and a clean end,
//! an `UNAUTHENTICATED` answer or a spent schedule ends the subscription for good. That answer
//! does not fire the session-expired hook from here — it writes signals, and the attached feed
//! reports the same expiry itself. [`WatchHandle::end`] reports which of those it was.
//!
//! **Agent tab keys are validated, not assumed.** Both `list_agents` and the stream join the
//! agents with one `GetLayout` per space ([`crate::net::agent_layouts`]). The stream caches each
//! space's layout, and a changed agent reuses it only while it is younger than the layout poll
//! ([`LAYOUT_FRESH`]) and still names the agent's tab as the event does; otherwise that space is
//! re-read first. A space whose read fails keeps its rows at their last known key (empty only
//! if never known) — the poll path reports that failure once, the stream logs it.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tonic::{Code, Status};

use crate::agents::{AgentStatus, agent_sort_key};
use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::terminal::data::feeds::grpc_feed::RECONNECT_BACKOFF;
use crate::features::terminal::domain::entities::{
    AgentEntry, AgentEventEntry, AgentMark, PaneEntry, ScrollStep, Space, TabEntry, TermLine,
};
use crate::features::terminal::domain::repositories::{
    AgentSink, TerminalRepository, WatchEnd, WatchHandle,
};
use crate::net::agent_layouts::{
    AgentLayouts, TabSlot, agent_spaces, fetch_agent_layouts, tab_label, unique_tab_key,
};
use crate::net::proto::{
    AgentEvent, AgentList, AgentMark as WireAgentMark, AgentMsg, CreateSpaceReq, Layout,
    NewPaneReq, NewTabReq, PaneMsg, PaneTarget, RenamePaneReq, RenameSpaceReq, RenameTabReq,
    ScrollDirection, ScrollReq, SessionRef, SpaceList, SwitchSpaceReq, TabTarget,
    ToggleFullscreenReq, agent_event,
};
use crate::net::{MuxrClient, error};

/// [`GrpcTerminalRepository::client`]'s failure detail when nothing is
/// connected.
const NOT_CONNECTED: &str = "not connected to a server";

/// The failure detail for a switch attempted with no session bound — a bug (the
/// sheet only switches on a screen that carries a session), so
/// [`AppFailure::Internal`] rather than a retryable network one.
const NO_SESSION: &str = "no session is bound to this terminal repository";

/// The failure detail for a blank detach id — defence in depth, matching the
/// mock's own enforcement point.
const BLANK_SESSION: &str = "detach_session called with a blank id";

/// How long a cached space layout may key a streamed change: the layout poll's own interval,
/// the staleness the tab list itself already accepts.
const LAYOUT_FRESH: Duration = Duration::from_secs(3);

/// Every agent tab key this repository has resolved, by `(space id, tab id)` — what a row
/// keeps while its space's layout cannot be read.
type KnownKeys = HashMap<(String, u64), String>;

/// The real [`TerminalRepository`], over a live `muxrd` connection. See the
/// [module docs](self).
pub struct GrpcTerminalRepository {
    connection: Arc<ConnectionController>,
    /// What every request addresses. A `Mutex` (not a `RefCell`) because this
    /// travels behind an `Arc<dyn TerminalRepository + Send + Sync>`.
    binding: Mutex<Binding>,
    /// Shared by `list_agents` and every stream this repository opens.
    known_keys: Arc<Mutex<KnownKeys>>,
}

/// The session and per-connection routing token
/// [`TerminalRepository::bind`] records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Binding {
    /// The attached session's backend-qualified id, or `None` on a bare
    /// `/terminal`.
    session: Option<String>,
    /// The `AttachTerminal` stream's minted id, echoed back for per-connection
    /// routing. Empty is the wire's own "legacy/fallback routing".
    connection_id: String,
}

impl GrpcTerminalRepository {
    /// A repository over the app's one [`ConnectionController`] — resolved once by
    /// `di::provide`. Unbound until the screen calls [`TerminalRepository::bind`].
    pub fn new(connection: Arc<ConnectionController>) -> Self {
        Self {
            connection,
            binding: Mutex::new(Binding::default()),
            known_keys: Arc::new(Mutex::new(KnownKeys::new())),
        }
    }

    /// The live client, or [`AppFailure::Network`] if nothing is connected.
    fn client(&self) -> Result<MuxrClient, AppFailure> {
        self.connection
            .client()
            .ok_or_else(|| AppFailure::Network(NOT_CONNECTED.to_string()))
    }

    /// The current binding. Poisoning is recovered from rather than propagated — a
    /// panic elsewhere must not make a live terminal permanently unaddressable.
    fn binding(&self) -> MutexGuard<'_, Binding> {
        self.binding.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The bound session id and routing token, or [`AppFailure::Internal`] when no
    /// session is bound — the shape every *switch* needs, as opposed to the reads,
    /// which answer empty instead.
    fn addressed(&self) -> Result<(String, String), AppFailure> {
        let binding = self.binding();
        match &binding.session {
            Some(session) => Ok((session.clone(), binding.connection_id.clone())),
            None => Err(AppFailure::Internal(NO_SESSION.to_string())),
        }
    }

    /// A [`SessionRef`] for the bound session, or `None` when nothing is bound. `space_id` is
    /// always empty here — [`list_tabs_in_space`](TerminalRepository::list_tabs_in_space) is the
    /// one caller that overwrites it before sending.
    fn session_ref(&self) -> Option<SessionRef> {
        let binding = self.binding();
        binding.session.as_ref().map(|session| SessionRef {
            session: session.clone(),
            connection_id: binding.connection_id.clone(),
            space_id: String::new(),
        })
    }

    /// Map a gRPC status through [`crate::net::error::from_status`], and — for
    /// `UNAUTHENTICATED` specifically — additionally invoke the shared
    /// session-expired hook.
    fn map_status(&self, client: &MuxrClient, status: Status) -> AppFailure {
        let failure = error::from_status(&status);
        if matches!(failure, AppFailure::Auth(_)) {
            self.connection.on_session_expired_for(client);
        }
        failure
    }

    /// The handle and the unspawned pump of one agents subscription, napping through `nap` on
    /// `schedule` and reading layouts from `source` — `watch_agents` spawns it; a test drives
    /// it on its own runtime.
    fn agent_watch(
        &self,
        sink: AgentSink,
        nap: Nap,
        schedule: Vec<Duration>,
        source: LayoutSource,
    ) -> Result<(WatchHandle, impl Future<Output = ()> + Send + 'static), AppFailure> {
        let request = self
            .session_ref()
            .ok_or_else(|| AppFailure::Internal(NO_SESSION.to_string()))?;
        self.client()?;
        let watch = Arc::new(AgentWatch::default());
        let stop = Arc::clone(&watch);
        let handle = WatchHandle::new(
            Arc::clone(&watch.live),
            Arc::clone(&watch.ended),
            move || stop.stop(),
        );
        let keys = StreamKeys {
            spaces: HashMap::new(),
            known: Arc::clone(&self.known_keys),
            source,
        };
        let pump = run_agent_watch(
            Arc::clone(&self.connection),
            request,
            Mutex::new(sink),
            watch,
            nap,
            schedule,
            keys,
        );
        Ok((handle, pump))
    }
}

/// Where the stream reads a space's layout from: the wire, or a test's script.
enum LayoutSource {
    Wire,
    #[cfg(test)]
    Scripted(Arc<tests::ScriptedLayouts>),
}

impl LayoutSource {
    async fn read(
        &self,
        client: &MuxrClient,
        request: &SessionRef,
        spaces: Vec<String>,
    ) -> AgentLayouts {
        match self {
            LayoutSource::Wire => fetch_agent_layouts(client, request, spaces).await,
            #[cfg(test)]
            LayoutSource::Scripted(script) => script.read(spaces),
        }
    }

    /// The clock layout freshness is judged on; a script may run it ahead.
    fn now(&self) -> Instant {
        match self {
            LayoutSource::Wire => Instant::now(),
            #[cfg(test)]
            LayoutSource::Scripted(script) => script.now(),
        }
    }
}

/// One space's layout as the stream last read it.
struct CachedSpace {
    slots: HashMap<u64, TabSlot>,
    read_at: Instant,
}

/// The stream's tab-key state: its per-space layout cache, the repository's known keys, and
/// where layouts come from.
struct StreamKeys {
    spaces: HashMap<String, CachedSpace>,
    known: Arc<Mutex<KnownKeys>>,
    source: LayoutSource,
}

impl StreamKeys {
    /// Read `spaces` and cache every one that answered; a failed one keeps its old entry.
    async fn reread(&mut self, client: &MuxrClient, request: &SessionRef, spaces: Vec<String>) {
        let layouts = self.source.read(client, request, spaces.clone()).await;
        let read_at = self.source.now();
        for failure in &layouts.failed {
            log::debug!(
                "agents stream: layout of space {:?} unread: {}",
                failure.space_id,
                failure.status.message()
            );
        }
        for space_id in spaces {
            if layouts.space_failed(&space_id) {
                continue;
            }
            let slots: HashMap<u64, TabSlot> = layouts
                .tabs
                .iter()
                .filter(|((space, _), _)| *space == space_id)
                .map(|((_, tab_id), slot)| (*tab_id, slot.clone()))
                .collect();
            lock_keys(&self.known)
                .retain(|(space, tab_id), _| *space != space_id || slots.contains_key(tab_id));
            self.spaces.insert(space_id, CachedSpace { slots, read_at });
        }
    }

    /// Whether `agent`'s space layout may key it as it stands: young enough, and naming the
    /// agent's tab as the event does.
    fn usable_for(&self, agent: &AgentMsg) -> bool {
        let now = self.source.now();
        self.spaces.get(&agent.space_id).is_some_and(|cached| {
            now.saturating_duration_since(cached.read_at) < LAYOUT_FRESH
                && cached
                    .slots
                    .get(&agent.tab_id)
                    .is_some_and(|slot| slot.name == agent.tab_name)
        })
    }

    /// `agent`'s tab key: its space's cached layout's (none when that layout lacks the tab),
    /// else — a space never read — the last one known, else empty.
    fn key_for(&self, agent: &AgentMsg) -> String {
        let mut known = lock_keys(&self.known);
        let place = (agent.space_id.clone(), agent.tab_id);
        let Some(cached) = self.spaces.get(&agent.space_id) else {
            return known.get(&place).cloned().unwrap_or_default();
        };
        let key = cached
            .slots
            .get(&agent.tab_id)
            .map(|slot| slot.tab_key.clone())
            .unwrap_or_default();
        if !key.is_empty() {
            known.insert(place, key.clone());
        }
        key
    }
}

fn lock_keys(known: &Mutex<KnownKeys>) -> MutexGuard<'_, KnownKeys> {
    known.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The state an agents subscription's pump shares with its [`WatchHandle`].
#[derive(Default)]
struct AgentWatch {
    stop: AtomicBool,
    wake: Notify,
    live: Arc<AtomicBool>,
    ended: Arc<AtomicU8>,
}

impl AgentWatch {
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    /// Resolves once [`stop`](Self::stop) has run; the stored permit makes a stop raised
    /// between the check and the wait unmissable.
    async fn stopped(&self) {
        while !self.stop.load(Ordering::Acquire) {
            self.wake.notified().await;
        }
    }
}

/// How one `WatchAgents` stream ended.
enum StreamEnd {
    /// A transport failure: reconnect, from the first step if this stream delivered.
    Dropped { delivered: bool },
    /// The server ended the stream: the subscription is over.
    Clean,
    /// The server refused the credential: the subscription is over.
    Unauthenticated,
}

/// How the pump naps between reconnects: production `tokio` has no timer, so the app naps on
/// frust's blocking pool, as the terminal feed does.
enum Nap {
    Frust,
    #[cfg(test)]
    Tokio,
}

impl Nap {
    async fn sleep(&self, delay: Duration) {
        match self {
            Nap::Frust => {
                let _ = frust::spawn_blocking(move || std::thread::sleep(delay)).await;
            }
            #[cfg(test)]
            Nap::Tokio => tokio::time::sleep(delay).await,
        }
    }
}

/// The subscription's whole life: stream, reconnect on `schedule`, until stopped or over.
/// The sink sits behind a `Mutex` only so a borrow of it may cross an `await`.
async fn run_agent_watch(
    connection: Arc<ConnectionController>,
    request: SessionRef,
    sink: Mutex<AgentSink>,
    watch: Arc<AgentWatch>,
    nap: Nap,
    schedule: Vec<Duration>,
    mut keys: StreamKeys,
) {
    let mut step = 0;
    loop {
        let attempt = watch_once(&connection, &request, &sink, &watch, &mut keys);
        let end = match race(watch.stopped(), attempt).await {
            Winner::First(()) => break,
            Winner::Second(end) => end,
        };
        watch.live.store(false, Ordering::Release);
        match end {
            StreamEnd::Clean => {
                WatchEnd::EndedClean.store(&watch.ended);
                break;
            }
            StreamEnd::Unauthenticated => {
                WatchEnd::EndedUnauthenticated.store(&watch.ended);
                break;
            }
            StreamEnd::Dropped { delivered: true } => step = 0,
            StreamEnd::Dropped { delivered: false } => {}
        }
        let Some(&delay) = schedule.get(step) else {
            WatchEnd::Exhausted.store(&watch.ended);
            break;
        };
        WatchEnd::Reconnecting.store(&watch.ended);
        step += 1;
        if let Winner::First(()) = race(watch.stopped(), nap.sleep(delay)).await {
            break;
        }
    }
    watch.live.store(false, Ordering::Release);
}

/// One `WatchAgents` stream, from open to end, each event mapped into `sink`.
async fn watch_once(
    connection: &ConnectionController,
    request: &SessionRef,
    sink: &Mutex<AgentSink>,
    watch: &AgentWatch,
    keys: &mut StreamKeys,
) -> StreamEnd {
    let Some(client) = connection.client() else {
        return StreamEnd::Dropped { delivered: false };
    };
    let mut stream = match client.authed().watch_agents(request.clone()).await {
        Ok(response) => response.into_inner(),
        Err(status) => return stream_end(&status, false),
    };
    WatchEnd::Open.store(&watch.ended);
    watch.live.store(true, Ordering::Release);
    let mut delivered = false;
    loop {
        match stream.message().await {
            Ok(Some(event)) => {
                delivered = true;
                if let Some(entry) = map_agent_event(&client, request, event, keys).await {
                    (sink.lock().unwrap_or_else(PoisonError::into_inner))(entry);
                }
            }
            Ok(None) => return StreamEnd::Clean,
            Err(status) => return stream_end(&status, delivered),
        }
    }
}

/// An error `status` as the end of a stream: an expired credential is over, anything else one
/// more drop.
fn stream_end(status: &Status, delivered: bool) -> StreamEnd {
    if status.code() == Code::Unauthenticated {
        StreamEnd::Unauthenticated
    } else {
        StreamEnd::Dropped { delivered }
    }
}

/// One wire event as an [`AgentEventEntry`], keyed through `keys` (see the [module
/// docs](self)): a snapshot re-reads every space it names, a change re-reads its own space
/// unless the cached layout is still usable for it. Events are mapped one at a time, so a
/// later event for the same space waits on, then reuses, the read an earlier one started.
/// `None` for an event kind this client does not know.
async fn map_agent_event(
    client: &MuxrClient,
    request: &SessionRef,
    event: AgentEvent,
    keys: &mut StreamKeys,
) -> Option<AgentEventEntry> {
    match event.kind? {
        agent_event::Kind::Snapshot(list) => {
            keys.spaces.clear();
            keys.reread(client, request, agent_spaces(&list.agents))
                .await;
            let mut rows: Vec<AgentEntry> = list
                .agents
                .into_iter()
                .map(|agent| {
                    let key = keys.key_for(&agent);
                    map_agent(agent, key)
                })
                .collect();
            rows.sort_by_key(|row| agent_sort_key(row.status, row.change_seq));
            Some(AgentEventEntry::Snapshot(rows))
        }
        agent_event::Kind::Changed(agent) => {
            if !keys.usable_for(&agent) {
                keys.reread(client, request, vec![agent.space_id.clone()])
                    .await;
            }
            let key = keys.key_for(&agent);
            Some(AgentEventEntry::Changed(Box::new(map_agent(agent, key))))
        }
        agent_event::Kind::RemovedPaneId(pane_id) => Some(AgentEventEntry::Removed(pane_id)),
    }
}

/// Which of [`race`]'s two futures finished first.
enum Winner<A, B> {
    First(A),
    Second(B),
}

/// Poll `first`, then `second`, finishing with whichever completes first — production `tokio`
/// carries no `select!`. `first` wins a tie.
async fn race<A: Future, B: Future>(first: A, second: B) -> Winner<A::Output, B::Output> {
    let mut first = std::pin::pin!(first);
    let mut second = std::pin::pin!(second);
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(value) = first.as_mut().poll(cx) {
            return Poll::Ready(Winner::First(value));
        }
        if let Poll::Ready(value) = second.as_mut().poll(cx) {
            return Poll::Ready(Winner::Second(value));
        }
        Poll::Pending
    })
    .await
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl TerminalRepository for GrpcTerminalRepository {
    fn bind(&self, session: Option<String>, connection_id: Option<String>) {
        let mut binding = self.binding();
        if binding.session != session {
            // Another session's tabs are another world: its keys must not resurrect here.
            lock_keys(&self.known_keys).clear();
        }
        binding.session = session;
        binding.connection_id = connection_id.unwrap_or_default();
    }

    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
        // Nothing bound, nothing to list — a bare `/terminal` is a reachable
        // screen, not a failure (see the module docs).
        let Some(request) = self.session_ref() else {
            return Ok(Vec::new());
        };
        let client = self.client()?;
        let response = client
            .authed()
            .get_spaces(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        Ok(map_spaces(response))
    }

    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
        let Some(request) = self.session_ref() else {
            return Ok(Vec::new());
        };
        let client = self.client()?;
        let response = client
            .authed()
            .get_layout(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        Ok(map_tabs(response))
    }

    async fn list_tabs_in_space(&self, space_id: &str) -> Result<Vec<TabEntry>, AppFailure> {
        let Some(mut request) = self.session_ref() else {
            return Ok(Vec::new());
        };
        request.space_id = space_id.to_string();
        let client = self.client()?;
        let response = client
            .authed()
            .get_layout(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        Ok(map_tabs(response))
    }

    async fn list_agents(&self) -> Result<Vec<AgentEntry>, AppFailure> {
        let Some(request) = self.session_ref() else {
            return Ok(Vec::new());
        };
        let client = self.client()?;
        let list = client
            .authed()
            .list_agents(request.clone())
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        // `AgentMsg` carries a tab id and a tab *name*, but `TabEntry::key` is derived from the
        // tab's name and position within its space's layout, so the key needs that layout: one
        // `GetLayout` per distinct space (the empty space id, a backend without spaces, reads
        // the focused space). A failed space keeps its rows at their last known key, and the
        // failure is reported once rather than failing the inventory.
        let layouts = fetch_agent_layouts(&client, &request, agent_spaces(&list.agents)).await;
        if let Some(first) = layouts.failed.first() {
            let failure = self.map_status(&client, first.status.clone());
            log::warn!(
                "list_agents: {} space layout(s) unread, rows keep their last tab key: {failure}",
                layouts.failed.len()
            );
        }
        Ok(map_agents(list, &layouts, &mut lock_keys(&self.known_keys)))
    }

    fn watch_agents(&self, sink: AgentSink) -> Result<WatchHandle, AppFailure> {
        let (handle, pump) = self.agent_watch(
            sink,
            Nap::Frust,
            RECONNECT_BACKOFF.to_vec(),
            LayoutSource::Wire,
        )?;
        frust::spawn(pump);
        Ok(handle)
    }

    async fn tab_lines(&self, _key: String) -> Result<Vec<TermLine>, AppFailure> {
        // The emulator owns a live session's content — see the module docs'
        // "Two calls that never reach the wire".
        Ok(Vec::new())
    }

    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = SwitchSpaceReq {
            session,
            space_id,
            connection_id,
        };
        let ack = client
            .authed()
            .switch_space(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = TabTarget {
            session,
            tab_id,
            connection_id,
        };
        let ack = client
            .authed()
            .go_to_tab(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
        if id.trim().is_empty() {
            return Err(AppFailure::Internal(BLANK_SESSION.to_string()));
        }
        // No RPC: the stream's teardown *is* the detach — see the module docs'
        // "Two calls that never reach the wire".
        Ok(())
    }

    async fn create_space(&self, label: String) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = create_space_request(session, connection_id, label);
        let ack = client
            .authed()
            .create_space(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn rename_space(&self, space_id: String, label: String) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = rename_space_request(session, connection_id, space_id, label);
        let ack = client
            .authed()
            .rename_space(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn new_tab(&self, name: String) -> Result<(), AppFailure> {
        // `NewTabReq` carries no `connection_id` — see the module docs'
        // "What it addresses".
        let (session, _connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = new_tab_request(session, name);
        let ack = client
            .authed()
            .new_tab(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn rename_tab(&self, tab_id: u64, name: String) -> Result<(), AppFailure> {
        let (session, _connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = rename_tab_request(session, tab_id, name);
        let ack = client
            .authed()
            .rename_tab(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn new_pane(&self, floating: bool) -> Result<(), AppFailure> {
        let (session, _connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = new_pane_request(session, floating);
        let ack = client
            .authed()
            .new_pane(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn rename_pane(
        &self,
        pane_id: u32,
        is_plugin: bool,
        name: String,
    ) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = rename_pane_request(session, connection_id, pane_id, is_plugin, name);
        let ack = client
            .authed()
            .rename_pane(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn focus_pane(&self, pane_id: u32, is_plugin: bool) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = pane_target(session, connection_id, pane_id, is_plugin);
        let ack = client
            .authed()
            .focus_pane(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn toggle_pane_fullscreen(
        &self,
        pane_id: u32,
        is_plugin: bool,
    ) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        // No floating hint: the relay runs its own live floating-state query, since the app
        // does not model floating visibility.
        let request = ToggleFullscreenReq {
            target: Some(pane_target(session, connection_id, pane_id, is_plugin)),
            target_is_floating: false,
            floating_visible: false,
            target_is_focused_floating: false,
            has_floating_hint: false,
        };
        let ack = client
            .authed()
            .toggle_pane_fullscreen(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn scroll_pane(
        &self,
        pane_id: u32,
        is_plugin: bool,
        direction: ScrollStep,
    ) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = scroll_pane_request(session, connection_id, pane_id, is_plugin, direction);
        let ack = client
            .authed()
            .scroll_pane(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }
}

// proto → entity mapping (pure; unit-tested against directly constructed
// messages, no server fixture).

/// A `GetSpaces` answer as the sheet's `spaces` pill row. The wire's `active` is
/// the *per-connection* answer (the relay overrides it for the connection that
/// last switched), which is why the sheet shows one selection per client.
fn map_spaces(list: SpaceList) -> Vec<Space> {
    list.spaces
        .into_iter()
        .map(|space| Space {
            id: space.id,
            name: space.name,
            selected: space.active,
        })
        .collect()
}

/// A `GetLayout` answer as the sheet's `tabs` pill row and the card's status row:
/// one [`TabEntry`] per tab, in wire order. `position` is 0-indexed on the wire and
/// 1-based on the pill, and every pane fact comes from the tab's *focused* pane
/// (falling back to its first) — the one the status row describes and the attached
/// stream is carrying.
fn map_tabs(layout: Layout) -> Vec<TabEntry> {
    let mut taken: Vec<String> = Vec::with_capacity(layout.tabs.len());
    layout
        .tabs
        .into_iter()
        .map(|tab| {
            let index = tab.position.saturating_add(1);
            let key = unique_tab_key(&tab.name, index, &mut taken);
            let pane = focused_pane(&tab.panes);
            TabEntry {
                index,
                tab_id: u64::from(tab.tab_id),
                key,
                label: tab_label(&tab.name, index),
                // A live pane carries a title and a working directory, and nothing
                // that corresponds to the mock's separate "name": the title *is*
                // the name a real session shows.
                pane_title: pane.map(|pane| pane.title.clone()).unwrap_or_default(),
                pane_name: pane.map(|pane| pane.title.clone()).unwrap_or_default(),
                pane_path: pane.map(|pane| pane.cwd.clone()).unwrap_or_default(),
                panes: tab.panes.iter().map(map_pane).collect(),
                active: tab.active,
                has_bell: tab.has_bell,
                floating_panes_visible: tab.floating_panes_visible,
                fullscreen_active: tab.fullscreen_active,
            }
        })
        .collect()
}

/// One `PaneMsg` as [`PaneEntry`] — a field-for-field carry, in wire order. The
/// tab around it derives extra identifiers but a pane does not need to: the wire's
/// own `PaneMsg.id` is already what the pane-targeted verbs address by.
fn map_pane(pane: &PaneMsg) -> PaneEntry {
    PaneEntry {
        id: pane.id,
        title: pane.title.clone(),
        cwd: pane.cwd.clone(),
        focused: pane.is_focused,
        floating: pane.is_floating,
        exited: pane.exited,
        is_plugin: pane.is_plugin,
        is_fullscreen: pane.is_fullscreen,
        x: pane.x,
        y: pane.y,
        rows: pane.rows,
        cols: pane.cols,
        agent: pane.agent.as_ref().map(map_agent_mark),
    }
}

/// The wire's `AgentMark` as [`AgentMark`]; an empty name is "no name".
fn map_agent_mark(mark: &WireAgentMark) -> AgentMark {
    AgentMark {
        kind: mark.kind.clone(),
        name: non_empty(&mark.name),
        status: AgentStatus::from_wire(mark.status),
    }
}

/// The wire spells an absent optional string as empty.
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// A `ListAgents` answer as [`AgentEntry`] rows, sorted by [`agent_sort_key`]. An agent's
/// `tab_key` is its tab's slot in `layouts` (so it equals the [`TabEntry::key`] a tab list
/// yields for the same tab), recorded in `known` (which a successful read of a space prunes of
/// tabs the read no longer lists); a row whose space failed to read keeps the
/// key `known` last held, and one whose space read fine but lacks its tab carries none.
fn map_agents(list: AgentList, layouts: &AgentLayouts, known: &mut KnownKeys) -> Vec<AgentEntry> {
    // A space whose layout read fine is authoritative: known keys of tabs it no longer lists go.
    let read_spaces: Vec<String> = agent_spaces(&list.agents)
        .into_iter()
        .filter(|space| !layouts.space_failed(space))
        .collect();
    known.retain(|(space, tab_id), _| {
        !read_spaces.contains(space) || layouts.tabs.contains_key(&(space.clone(), *tab_id))
    });
    let mut rows: Vec<AgentEntry> = list
        .agents
        .into_iter()
        .map(|agent| {
            let place = (agent.space_id.clone(), agent.tab_id);
            let key = match layouts.tabs.get(&place) {
                Some(slot) => {
                    known.insert(place, slot.tab_key.clone());
                    slot.tab_key.clone()
                }
                None if layouts.space_failed(&agent.space_id) => {
                    known.get(&place).cloned().unwrap_or_default()
                }
                None => String::new(),
            };
            map_agent(agent, key)
        })
        .collect();
    rows.sort_by_key(|row| agent_sort_key(row.status, row.change_seq));
    rows
}

fn map_agent(agent: AgentMsg, tab_key: String) -> AgentEntry {
    AgentEntry {
        pane_id: agent.pane_id,
        tab_id: agent.tab_id,
        tab_key,
        space_id: non_empty(&agent.space_id),
        space_name: non_empty(&agent.space_name),
        tab_name: agent.tab_name,
        kind: agent.kind,
        name: non_empty(&agent.name),
        display_kind: non_empty(&agent.display_kind),
        status: AgentStatus::from_wire(agent.status),
        title: non_empty(&agent.title),
        summary: non_empty(&agent.summary),
        cwd: non_empty(&agent.cwd),
        focused: agent.focused,
        change_seq: agent.change_seq,
    }
}

/// [`ScrollStep`] onto the wire's `ScrollDirection` — only the two notch values
/// this screen sends; `domain::entities::ScrollStep` records why the wire's other
/// five directions are not reachable yet.
fn map_scroll_step(step: ScrollStep) -> ScrollDirection {
    match step {
        ScrollStep::Up => ScrollDirection::Up,
        ScrollStep::Down => ScrollDirection::Down,
    }
}

// Workspace-verb request construction (pure; unit-tested directly against the
// values they carry).

/// A [`PaneTarget`] addressing `pane_id` over `session`/`connection_id`.
fn pane_target(
    session: String,
    connection_id: String,
    pane_id: u32,
    is_plugin: bool,
) -> PaneTarget {
    PaneTarget {
        session,
        pane_id,
        is_plugin,
        connection_id,
    }
}

/// `CreateSpace`'s request.
fn create_space_request(session: String, connection_id: String, label: String) -> CreateSpaceReq {
    CreateSpaceReq {
        session,
        label,
        connection_id,
    }
}

/// `RenameSpace`'s request.
fn rename_space_request(
    session: String,
    connection_id: String,
    space_id: String,
    label: String,
) -> RenameSpaceReq {
    RenameSpaceReq {
        session,
        space_id,
        label,
        connection_id,
    }
}

/// `NewTab`'s request; `NewTabReq` carries no `connection_id`, unlike
/// [`create_space_request`]'s own `label` field.
fn new_tab_request(session: String, tab_name: String) -> NewTabReq {
    NewTabReq { session, tab_name }
}

/// `RenameTab`'s request — `RenameTabReq` carries no `connection_id`.
fn rename_tab_request(session: String, tab_id: u64, name: String) -> RenameTabReq {
    RenameTabReq {
        session,
        tab_id,
        name,
    }
}

/// `NewPane`'s request — always unnamed (the sheet's new-pane row has no naming
/// dialog, unlike [`new_tab_request`]'s); `NewPaneReq` carries no `connection_id`.
fn new_pane_request(session: String, floating: bool) -> NewPaneReq {
    NewPaneReq {
        session,
        floating,
        pane_name: String::new(),
    }
}

/// `RenamePane`'s request.
fn rename_pane_request(
    session: String,
    connection_id: String,
    pane_id: u32,
    is_plugin: bool,
    name: String,
) -> RenamePaneReq {
    RenamePaneReq {
        target: Some(pane_target(session, connection_id, pane_id, is_plugin)),
        name,
    }
}

/// `ScrollPane`'s request.
fn scroll_pane_request(
    session: String,
    connection_id: String,
    pane_id: u32,
    is_plugin: bool,
    direction: ScrollStep,
) -> ScrollReq {
    ScrollReq {
        target: Some(pane_target(session, connection_id, pane_id, is_plugin)),
        direction: map_scroll_step(direction) as i32,
    }
}

/// The pane a tab's status row describes: the focused one, or — for a tab the
/// backend reports with nothing focused — its first.
fn focused_pane(panes: &[PaneMsg]) -> Option<&PaneMsg> {
    panes
        .iter()
        .find(|pane| pane.is_focused)
        .or_else(|| panes.first())
}

#[cfg(test)]
mod tests {
    use frust::GetUntracked;
    use zeroize::Zeroizing;

    use crate::connection::{
        ConnectRun, ConnectTarget, ConnectionBackend, NetConnectionBackend, TrustMode,
    };
    use crate::net::proto::{ActionAck, PaneMsg, Space as WireSpace, TabMsg};
    use crate::net::testing::{self, MuxrdFixture, wait_until};

    use super::*;

    /// Drive one repository call to completion on a current-thread runtime —
    /// no frust runtime in play, mirroring the mock repository's own tests.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    /// A disconnected repository, bound to `session` — the shape every
    /// no-wire test below needs.
    fn repo(session: Option<&str>) -> GrpcTerminalRepository {
        let repo = GrpcTerminalRepository::new(Arc::new(ConnectionController::detached()));
        repo.bind(session.map(str::to_string), None);
        repo
    }

    fn wire_space(id: &str, name: &str, active: bool) -> WireSpace {
        WireSpace {
            id: id.to_string(),
            name: name.to_string(),
            active,
        }
    }

    fn pane(title: &str, cwd: &str, is_focused: bool) -> PaneMsg {
        PaneMsg {
            id: 0,
            agent: None,
            title: title.to_string(),
            is_focused,
            is_floating: false,
            exited: false,
            command: String::new(),
            cwd: cwd.to_string(),
            x: 0,
            y: 0,
            rows: 0,
            cols: 0,
            is_plugin: false,
            is_fullscreen: false,
        }
    }

    fn tab(position: u32, tab_id: u32, name: &str, active: bool, panes: Vec<PaneMsg>) -> TabMsg {
        TabMsg {
            position,
            name: name.to_string(),
            active,
            has_bell: tab_id == BELL_TAB_ID,
            panes_to_hide: 0,
            tab_id,
            panes,
            fullscreen_active: false,
            floating_panes_visible: false,
        }
    }

    // -- proto → entity mapping ----------------------------------------------

    #[test]
    fn a_panes_agent_mark_maps_and_an_unknown_status_reads_unknown() {
        use crate::net::proto::{AgentMark as WireMark, AgentStatus as WireStatus};
        let mut wire = pane("claude", "/srv", true);
        wire.agent = Some(WireMark {
            kind: "claude".to_string(),
            name: String::new(),
            status: WireStatus::Blocked as i32,
        });
        let mark = map_pane(&wire).agent.expect("mark");
        assert_eq!(mark.kind, "claude");
        assert_eq!(mark.name, None);
        assert_eq!(mark.status, AgentStatus::Blocked);

        wire.agent = Some(WireMark {
            kind: "x".to_string(),
            name: "bob".to_string(),
            status: 99,
        });
        let mark = map_pane(&wire).agent.expect("mark");
        assert_eq!(mark.name.as_deref(), Some("bob"));
        assert_eq!(mark.status, AgentStatus::Unknown);
        assert_eq!(map_pane(&pane("zsh", "/", false)).agent, None);
    }

    fn agent_msg(pane_id: u32, tab_id: u64, status: i32, change_seq: u64) -> AgentMsg {
        AgentMsg {
            pane_id,
            tab_id,
            space_id: String::new(),
            space_name: String::new(),
            tab_name: "zsh".to_string(),
            kind: "claude".to_string(),
            name: String::new(),
            display_kind: String::new(),
            status,
            title: "t".to_string(),
            summary: String::new(),
            cwd: "/srv".to_string(),
            focused: false,
            change_seq,
            ..Default::default()
        }
    }

    #[test]
    fn an_agents_tab_key_equals_the_tab_entry_key_for_the_same_tab() {
        use crate::net::proto::AgentStatus as WireStatus;
        // Two tabs named `zsh`: the second's key is disambiguated by position.
        let tabs = map_tabs(Layout {
            tabs: vec![
                tab(0, 7, "zsh", true, vec![pane("a", "/", true)]),
                tab(1, 8, "zsh", false, vec![pane("b", "/", true)]),
            ],
        });
        let layouts = AgentLayouts {
            tabs: crate::net::agent_layouts::layout_slots(&Layout {
                tabs: vec![
                    tab(0, 7, "zsh", true, Vec::new()),
                    tab(1, 8, "zsh", false, Vec::new()),
                ],
            })
            .into_iter()
            .map(|(tab_id, slot)| ((String::new(), tab_id), slot))
            .collect(),
            failed: Vec::new(),
        };
        let list = AgentList {
            agents: vec![
                agent_msg(1, 7, WireStatus::Idle as i32, 1),
                agent_msg(2, 8, WireStatus::Blocked as i32, 2),
                agent_msg(3, 8, 99, 3),
            ],
            summary: None,
        };
        let rows = map_agents(list, &layouts, &mut KnownKeys::new());
        assert_eq!(rows[0].status, AgentStatus::Blocked, "most urgent first");
        assert_eq!(rows[0].tab_key, tabs[1].key);
        assert_eq!(rows[1].status, AgentStatus::Idle);
        assert_eq!(rows[1].tab_key, tabs[0].key);
        assert_eq!(rows[2].status, AgentStatus::Unknown);
        assert_eq!(rows[0].space_id, None);
        assert_eq!(rows[0].title.as_deref(), Some("t"));
        assert_eq!(rows[0].subtitle(), Some("t"));
        assert_eq!(rows[0].display_name(), "claude");
    }

    // -- the agents stream, against the in-process fixture ----------------------

    /// Two short steps, so a reconnect costs milliseconds.
    const TEST_BACKOFF: [Duration; 2] = [Duration::from_millis(5), Duration::from_millis(5)];

    /// A repository bound to the fixture's `alpha` session over a real pinned, logged-in dial —
    /// driven here rather than through `connect`, whose `frust::spawn` has no runtime under a
    /// plain `#[tokio::test]`.
    async fn fixture_repo(fixture: &MuxrdFixture) -> GrpcTerminalRepository {
        let backend = Arc::new(NetConnectionBackend::new());
        let target = ConnectTarget {
            host: "127.0.0.1".to_string(),
            port: fixture.port(),
            trust_mode: TrustMode::Pinned,
            label: "Fixture".to_string(),
            offer_credential: false,
            token: Zeroizing::new(testing::AUTH_TOKEN.to_string()),
            remember: false,
            fingerprint: Some(fixture.fingerprint_hex().to_string()),
        };
        let ConnectRun::Pending(work) = backend.begin_connect(target) else {
            panic!("a real dial is never decided before it runs");
        };
        work.await.expect("the fixture accepts its own pin + token");
        let repo =
            GrpcTerminalRepository::new(Arc::new(ConnectionController::with_backend(backend)));
        repo.bind(Some(testing::SESSION_NAMES[0].to_string()), None);
        repo
    }

    type Seen = Arc<Mutex<Vec<AgentEventEntry>>>;

    /// A sink that records every event it is handed.
    fn recorder() -> (AgentSink, Seen) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let sink: AgentSink = Box::new(move |event| sink_seen.lock().expect("ok").push(event));
        (sink, seen)
    }

    fn event(kind: agent_event::Kind) -> AgentEvent {
        AgentEvent { kind: Some(kind) }
    }

    #[test]
    fn a_watch_needs_a_bound_session_and_a_live_client() {
        let unbound =
            repo(None).agent_watch(recorder().0, Nap::Tokio, Vec::new(), LayoutSource::Wire);
        assert!(matches!(unbound.err(), Some(AppFailure::Internal(_))));
        let offline = repo(Some("alpha")).agent_watch(
            recorder().0,
            Nap::Tokio,
            Vec::new(),
            LayoutSource::Wire,
        );
        assert!(matches!(offline.err(), Some(AppFailure::Network(_))));
    }

    #[tokio::test]
    async fn a_watch_maps_snapshot_change_and_removal_and_its_drop_ends_the_stream() {
        use crate::net::proto::AgentStatus as WireStatus;
        let fixture = MuxrdFixture::start().await;
        let script = fixture.agents();
        let repo = fixture_repo(&fixture).await;
        let (sink, seen) = recorder();
        let (handle, pump) = repo
            .agent_watch(sink, Nap::Tokio, TEST_BACKOFF.to_vec(), LayoutSource::Wire)
            .expect("bound and connected");
        let pump = tokio::spawn(pump);
        wait_until("the stream opens", || {
            script.opened() == 1 && handle.is_live()
        })
        .await;

        let list = AgentList {
            agents: vec![agent_msg(1, 7, WireStatus::Working as i32, 1)],
            summary: None,
        };
        script.push(event(agent_event::Kind::Snapshot(list)));
        script.push(event(agent_event::Kind::Changed(agent_msg(
            2,
            7,
            WireStatus::Blocked as i32,
            2,
        ))));
        script.push(event(agent_event::Kind::RemovedPaneId(1)));
        wait_until("three events land", || seen.lock().expect("ok").len() == 3).await;
        let seen = seen.lock().expect("ok").clone();
        let AgentEventEntry::Snapshot(rows) = &seen[0] else {
            panic!("a snapshot first: {seen:?}");
        };
        assert_eq!((rows.len(), rows[0].pane_id), (1, 1));
        assert!(
            matches!(&seen[1], AgentEventEntry::Changed(agent)
                if agent.pane_id == 2 && agent.status == AgentStatus::Blocked),
            "{seen:?}"
        );
        assert_eq!(seen[2], AgentEventEntry::Removed(1));

        drop(handle);
        wait_until("the pump ends", || pump.is_finished()).await;
        wait_until("the server sees the client go", || script.client_gone()).await;
        assert_eq!(script.opened(), 1, "a stop never reconnects");
    }

    #[tokio::test]
    async fn a_failed_stream_reconnects_and_a_clean_end_stays_ended() {
        let fixture = MuxrdFixture::start().await;
        let script = fixture.agents();
        let repo = fixture_repo(&fixture).await;
        let (handle, pump) = repo
            .agent_watch(
                recorder().0,
                Nap::Tokio,
                TEST_BACKOFF.to_vec(),
                LayoutSource::Wire,
            )
            .expect("bound and connected");
        let pump = tokio::spawn(pump);
        wait_until("the stream opens", || script.opened() == 1).await;

        script.fail();
        wait_until("it reconnects", || script.opened() == 2 && handle.is_live()).await;
        assert_eq!(handle.end(), WatchEnd::Open);
        script.end();
        wait_until("a clean end is the last word", || pump.is_finished()).await;
        assert!(!handle.is_live());
        assert_eq!(handle.end(), WatchEnd::EndedClean);
        assert_eq!(script.opened(), 2);
    }

    #[tokio::test]
    async fn an_unauthenticated_watch_stops_without_reconnecting() {
        let fixture = MuxrdFixture::start().await;
        let script = fixture.agents();
        script.refuse_unauthenticated(true);
        let repo = fixture_repo(&fixture).await;
        let (handle, pump) = repo
            .agent_watch(
                recorder().0,
                Nap::Tokio,
                TEST_BACKOFF.to_vec(),
                LayoutSource::Wire,
            )
            .expect("bound and connected");
        tokio::spawn(pump).await.expect("the pump ends on its own");
        assert_eq!((script.refused(), script.opened()), (1, 0));
        assert!(!handle.is_live());
        assert_eq!(handle.end(), WatchEnd::EndedUnauthenticated);
    }

    #[tokio::test]
    async fn a_spent_reconnect_schedule_ends_the_watch_as_exhausted() {
        let fixture = MuxrdFixture::start().await;
        let script = fixture.agents();
        let repo = fixture_repo(&fixture).await;
        let (handle, pump) = repo
            .agent_watch(recorder().0, Nap::Tokio, Vec::new(), LayoutSource::Wire)
            .expect("bound and connected");
        let pump = tokio::spawn(pump);
        wait_until("the stream opens", || script.opened() == 1).await;
        script.fail();
        wait_until("the pump gives up", || pump.is_finished()).await;
        assert_eq!(handle.end(), WatchEnd::Exhausted);
    }

    /// Per-space layouts a test scripts for the stream's [`LayoutSource`], with a clock it can
    /// run ahead and a count of the reads it answered.
    #[derive(Default)]
    pub(super) struct ScriptedLayouts {
        layouts: Mutex<HashMap<String, Option<Layout>>>,
        reads: std::sync::atomic::AtomicUsize,
        ahead: Mutex<Duration>,
    }

    impl ScriptedLayouts {
        /// Answer `space`'s reads with `tabs`, or fail them when `None`.
        fn set(&self, space: &str, tabs: Option<Vec<TabMsg>>) {
            self.layouts
                .lock()
                .expect("ok")
                .insert(space.to_string(), tabs.map(|tabs| Layout { tabs }));
        }

        fn advance(&self, by: Duration) {
            *self.ahead.lock().expect("ok") += by;
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        pub(super) fn now(&self) -> Instant {
            Instant::now() + *self.ahead.lock().expect("ok")
        }

        pub(super) fn read(&self, spaces: Vec<String>) -> AgentLayouts {
            self.reads.fetch_add(spaces.len(), Ordering::SeqCst);
            let layouts = self.layouts.lock().expect("ok").clone();
            let read = crate::net::agent_layouts::join_layouts(spaces, |space| {
                let answer = layouts
                    .get(&space)
                    .cloned()
                    .flatten()
                    .ok_or_else(|| Status::unavailable("scripted layout failure"));
                std::future::ready(answer)
            });
            block_on_ready(read)
        }
    }

    /// Poll a future every part of which is already ready.
    fn block_on_ready<T>(fut: impl Future<Output = T>) -> T {
        let mut fut = std::pin::pin!(fut);
        let waker = std::task::Waker::noop();
        match fut
            .as_mut()
            .poll(&mut std::task::Context::from_waker(waker))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("a scripted read never waits"),
        }
    }

    /// A stream over the fixture whose layouts come from a fresh script, opened and live.
    async fn scripted_watch(
        fixture: &MuxrdFixture,
    ) -> (
        WatchHandle,
        Seen,
        Arc<ScriptedLayouts>,
        tokio::task::JoinHandle<()>,
    ) {
        let script = fixture.agents();
        let repo = fixture_repo(fixture).await;
        let layouts = Arc::new(ScriptedLayouts::default());
        let (sink, seen) = recorder();
        let (handle, pump) = repo
            .agent_watch(
                sink,
                Nap::Tokio,
                TEST_BACKOFF.to_vec(),
                LayoutSource::Scripted(Arc::clone(&layouts)),
            )
            .expect("bound and connected");
        let pump = tokio::spawn(pump);
        wait_until("the stream opens", || {
            script.opened() == 1 && handle.is_live()
        })
        .await;
        (handle, seen, layouts, pump)
    }

    /// An agent in pane `pane_id` of tab `tab_id`, named `tab_name` on the wire, in space `ws`.
    fn agent_in(pane_id: u32, tab_id: u64, tab_name: &str, change_seq: u64) -> AgentMsg {
        AgentMsg {
            space_id: "ws".to_string(),
            tab_name: tab_name.to_string(),
            ..agent_msg(pane_id, tab_id, 1, change_seq)
        }
    }

    /// The tab key the newest recorded `Changed` carries, once `count` events have landed.
    async fn nth_changed_key(seen: &Seen, count: usize) -> String {
        wait_until("the event lands", || {
            seen.lock().expect("ok").len() == count
        })
        .await;
        match seen.lock().expect("ok").last() {
            Some(AgentEventEntry::Changed(agent)) => agent.tab_key.clone(),
            other => panic!("a change: {other:?}"),
        }
    }

    fn snapshot_of(agents: Vec<AgentMsg>) -> AgentEvent {
        event(agent_event::Kind::Snapshot(AgentList {
            agents,
            summary: None,
        }))
    }

    fn changed(agent: AgentMsg) -> AgentEvent {
        event(agent_event::Kind::Changed(agent))
    }

    #[tokio::test]
    async fn after_a_tab_rename_the_next_change_carries_the_new_key() {
        let fixture = MuxrdFixture::start().await;
        let (_handle, seen, layouts, _pump) = scripted_watch(&fixture).await;
        let script = fixture.agents();
        layouts.set("ws", Some(vec![tab(0, 7, "zsh", true, Vec::new())]));
        script.push(snapshot_of(vec![agent_in(1, 7, "zsh", 1)]));
        wait_until("the snapshot", || seen.lock().expect("ok").len() == 1).await;

        layouts.set("ws", Some(vec![tab(0, 7, "build", true, Vec::new())]));
        script.push(changed(agent_in(1, 7, "build", 2)));
        assert_eq!(nth_changed_key(&seen, 2).await, "build");

        // A fresh layout naming the tab as the event does is reused, not re-read.
        let reads = layouts.reads();
        script.push(changed(agent_in(1, 7, "build", 3)));
        assert_eq!(nth_changed_key(&seen, 3).await, "build");
        assert_eq!(layouts.reads(), reads);
    }

    #[tokio::test]
    async fn after_a_tab_close_shifts_positions_the_next_change_carries_the_right_key() {
        let fixture = MuxrdFixture::start().await;
        let (_handle, seen, layouts, _pump) = scripted_watch(&fixture).await;
        let script = fixture.agents();
        layouts.set(
            "ws",
            Some(vec![
                tab(0, 6, "", true, Vec::new()),
                tab(1, 7, "", false, Vec::new()),
            ]),
        );
        script.push(snapshot_of(vec![agent_in(1, 7, "", 1)]));
        wait_until("the snapshot", || seen.lock().expect("ok").len() == 1).await;
        let first = seen.lock().expect("ok")[0].clone();
        let AgentEventEntry::Snapshot(rows) = first else {
            panic!("a snapshot first");
        };
        assert_eq!(rows[0].tab_key, "tab 2");

        // Tab 6 closes: tab 7 moves to the first position, its name unchanged. Once the cached
        // layout is older than the poll interval, the change re-reads it.
        layouts.set("ws", Some(vec![tab(0, 7, "", true, Vec::new())]));
        layouts.advance(LAYOUT_FRESH);
        script.push(changed(agent_in(1, 7, "", 2)));
        assert_eq!(nth_changed_key(&seen, 2).await, "tab 1");
    }

    #[tokio::test]
    async fn a_failed_layout_read_keeps_the_previous_key_and_still_delivers_the_row() {
        let fixture = MuxrdFixture::start().await;
        let (_handle, seen, layouts, _pump) = scripted_watch(&fixture).await;
        let script = fixture.agents();
        layouts.set("ws", Some(vec![tab(0, 7, "zsh", true, Vec::new())]));
        script.push(snapshot_of(vec![agent_in(1, 7, "zsh", 1)]));
        wait_until("the snapshot", || seen.lock().expect("ok").len() == 1).await;

        layouts.set("ws", None);
        script.push(changed(agent_in(1, 7, "renamed", 2)));
        assert_eq!(nth_changed_key(&seen, 2).await, "zsh");

        // A snapshot whose space fails keeps the row at its last known key too.
        script.push(snapshot_of(vec![agent_in(1, 7, "renamed", 3)]));
        wait_until("the snapshot", || seen.lock().expect("ok").len() == 3).await;
        let third = seen.lock().expect("ok")[2].clone();
        let AgentEventEntry::Snapshot(rows) = third else {
            panic!("a snapshot");
        };
        assert_eq!(rows[0].tab_key, "zsh");
    }

    #[test]
    fn a_listed_agent_in_an_unread_space_keeps_its_last_known_key_and_is_never_dropped() {
        use crate::net::agent_layouts::{SpaceFailure, TabSlot};
        let slot = TabSlot {
            tab_key: "zsh".to_string(),
            position: 0,
            name: "zsh".to_string(),
        };
        let read = AgentLayouts {
            tabs: HashMap::from([(("ws".to_string(), 7), slot)]),
            failed: Vec::new(),
        };
        let list = || AgentList {
            agents: vec![agent_in(1, 7, "zsh", 1), agent_in(2, 9, "x", 2)],
            summary: None,
        };
        let mut known = KnownKeys::new();
        let rows = map_agents(list(), &read, &mut known);
        let keys = |rows: &[AgentEntry]| -> Vec<(u32, String)> {
            let mut keys: Vec<_> = rows
                .iter()
                .map(|r| (r.pane_id, r.tab_key.clone()))
                .collect();
            keys.sort();
            keys
        };
        assert_eq!(
            keys(&rows),
            [(1, "zsh".to_string()), (2, String::new())],
            "a tab the read layout lacks carries no key"
        );

        let failed = AgentLayouts {
            tabs: HashMap::new(),
            failed: vec![SpaceFailure {
                space_id: "ws".to_string(),
                status: Status::unavailable("down"),
            }],
        };
        let rows = map_agents(list(), &failed, &mut known);
        assert_eq!(keys(&rows), [(1, "zsh".to_string()), (2, String::new())]);
    }

    #[tokio::test]
    async fn a_rebind_clears_the_known_keys_so_a_failed_read_cannot_resurrect_the_old_sessions_key()
    {
        let fixture = MuxrdFixture::start().await;
        let script = fixture.agents();
        let repo = fixture_repo(&fixture).await;
        let layouts = Arc::new(ScriptedLayouts::default());
        let (sink, seen) = recorder();
        let (handle, pump) = repo
            .agent_watch(
                sink,
                Nap::Tokio,
                TEST_BACKOFF.to_vec(),
                LayoutSource::Scripted(Arc::clone(&layouts)),
            )
            .expect("bound and connected");
        let pump = tokio::spawn(pump);
        wait_until("the stream opens", || {
            script.opened() == 1 && handle.is_live()
        })
        .await;
        layouts.set("ws", Some(vec![tab(0, 7, "zsh", true, Vec::new())]));
        script.push(snapshot_of(vec![agent_in(1, 7, "zsh", 1)]));
        wait_until("the snapshot", || seen.lock().expect("ok").len() == 1).await;
        assert_eq!(
            lock_keys(&repo.known_keys).get(&("ws".to_string(), 7)),
            Some(&"zsh".to_string())
        );

        drop(handle);
        wait_until("the pump ends", || pump.is_finished()).await;
        repo.bind(
            Some(testing::SESSION_NAMES[0].to_string()),
            Some("again".to_string()),
        );
        assert!(
            !lock_keys(&repo.known_keys).is_empty(),
            "the same session keeps its keys across a new connection id"
        );
        repo.bind(Some(testing::SESSION_NAMES[1].to_string()), None);
        assert!(lock_keys(&repo.known_keys).is_empty(), "another session");

        layouts.set("ws", None);
        let (sink, seen) = recorder();
        let (_handle, pump) = repo
            .agent_watch(
                sink,
                Nap::Tokio,
                TEST_BACKOFF.to_vec(),
                LayoutSource::Scripted(Arc::clone(&layouts)),
            )
            .expect("bound and connected");
        let _pump = tokio::spawn(pump);
        wait_until("the second stream opens", || script.opened() == 2).await;
        script.push(snapshot_of(vec![agent_in(1, 7, "zsh", 2)]));
        wait_until("the snapshot", || seen.lock().expect("ok").len() == 1).await;
        let AgentEventEntry::Snapshot(rows) = seen.lock().expect("ok")[0].clone() else {
            panic!("a snapshot");
        };
        assert_eq!(rows[0].tab_key, "", "the old session's key is gone");
    }

    #[tokio::test]
    async fn a_successful_layout_read_evicts_the_known_keys_of_tabs_it_no_longer_lists() {
        let fixture = MuxrdFixture::start().await;
        let script = fixture.agents();
        let repo = fixture_repo(&fixture).await;
        let layouts = Arc::new(ScriptedLayouts::default());
        let (sink, seen) = recorder();
        let (handle, pump) = repo
            .agent_watch(
                sink,
                Nap::Tokio,
                TEST_BACKOFF.to_vec(),
                LayoutSource::Scripted(Arc::clone(&layouts)),
            )
            .expect("bound and connected");
        let _pump = tokio::spawn(pump);
        wait_until("the stream opens", || {
            script.opened() == 1 && handle.is_live()
        })
        .await;
        layouts.set(
            "ws",
            Some(vec![
                tab(0, 7, "zsh", true, Vec::new()),
                tab(1, 8, "vim", false, Vec::new()),
            ]),
        );
        script.push(snapshot_of(vec![
            agent_in(1, 7, "zsh", 1),
            agent_in(2, 8, "vim", 1),
        ]));
        wait_until("the snapshot", || seen.lock().expect("ok").len() == 1).await;
        assert_eq!(lock_keys(&repo.known_keys).len(), 2);

        layouts.set("ws", Some(vec![tab(0, 7, "zsh", true, Vec::new())]));
        script.push(snapshot_of(vec![agent_in(1, 7, "zsh", 2)]));
        wait_until("the second snapshot", || {
            seen.lock().expect("ok").len() == 2
        })
        .await;
        let known: Vec<(String, u64)> = lock_keys(&repo.known_keys).keys().cloned().collect();
        assert_eq!(known, [("ws".to_string(), 7)], "the closed tab's key went");
    }

    #[test]
    fn a_list_prunes_known_keys_of_read_spaces_only() {
        use crate::net::agent_layouts::{SpaceFailure, TabSlot};
        let slot = TabSlot {
            tab_key: "zsh".to_string(),
            position: 0,
            name: "zsh".to_string(),
        };
        let list = || AgentList {
            agents: vec![agent_in(1, 7, "zsh", 1)],
            summary: None,
        };
        let seeded = || {
            KnownKeys::from([
                (("ws".to_string(), 7), "zsh".to_string()),
                (("ws".to_string(), 8), "vim".to_string()),
                (("elsewhere".to_string(), 1), "sh".to_string()),
            ])
        };
        let read = AgentLayouts {
            tabs: HashMap::from([(("ws".to_string(), 7), slot)]),
            failed: Vec::new(),
        };
        let mut known = seeded();
        map_agents(list(), &read, &mut known);
        let mut kept: Vec<_> = known.keys().cloned().collect();
        kept.sort();
        assert_eq!(
            kept,
            [("elsewhere".to_string(), 1), ("ws".to_string(), 7)],
            "tab 8 of the read space went; an unread space is untouched"
        );

        let failed = AgentLayouts {
            tabs: HashMap::new(),
            failed: vec![SpaceFailure {
                space_id: "ws".to_string(),
                status: Status::unavailable("down"),
            }],
        };
        let mut known = seeded();
        map_agents(list(), &failed, &mut known);
        assert_eq!(known.len(), 3, "a failed read prunes nothing");
    }

    #[test]
    fn an_empty_agent_list_maps_to_no_rows() {
        let list = AgentList {
            agents: Vec::new(),
            summary: None,
        };
        assert!(map_agents(list, &AgentLayouts::default(), &mut KnownKeys::new()).is_empty());
    }

    #[test]
    fn a_space_list_maps_its_id_name_and_per_connection_active_flag() {
        let spaces = map_spaces(SpaceList {
            spaces: vec![
                wire_space("ws-1", "cupline", false),
                wire_space("ws-2", "forgekit", true),
            ],
        });

        assert_eq!(spaces.len(), 2);
        assert_eq!(spaces[0].id, "ws-1");
        assert_eq!(spaces[0].name, "cupline");
        assert!(!spaces[0].selected);
        assert!(
            spaces[1].selected,
            "the wire's `active` is what the sheet paints selected"
        );
    }

    #[test]
    fn a_zellij_session_maps_its_empty_space_list_to_an_empty_pill_row() {
        assert!(map_spaces(SpaceList { spaces: Vec::new() }).is_empty());
    }

    #[test]
    fn a_layout_maps_one_tab_entry_per_tab_with_its_backend_id_and_pill_index() {
        let tabs = map_tabs(Layout {
            tabs: vec![
                tab(0, 7, "btop", false, vec![pane("htop", "/home/ed", true)]),
                tab(1, 9, "devbox", true, vec![pane("zsh", "/srv", true)]),
            ],
        });

        assert_eq!(tabs.len(), 2);
        assert_eq!(
            tabs[0].index, 1,
            "the wire counts from zero, the pill from one"
        );
        assert_eq!(tabs[0].tab_id, 7);
        assert_eq!(tabs[0].key, "btop");
        assert_eq!(tabs[0].pill_label(), "1 btop");
        assert!(!tabs[0].active);
        assert_eq!(tabs[1].index, 2);
        assert_eq!(tabs[1].tab_id, 9);
        assert!(tabs[1].active);
    }

    #[test]
    fn a_tabs_pane_facts_come_from_its_focused_pane() {
        let tabs = map_tabs(Layout {
            tabs: vec![tab(
                0,
                1,
                "dev",
                true,
                vec![
                    pane("first", "/first", false),
                    pane("ed@devbox", "/home/ed", true),
                ],
            )],
        });

        assert_eq!(tabs[0].pane_title, "ed@devbox");
        assert_eq!(tabs[0].pane_name, "ed@devbox");
        assert_eq!(tabs[0].pane_path, "/home/ed");
    }

    #[test]
    fn a_tab_with_nothing_focused_falls_back_to_its_first_pane_and_a_paneless_one_to_blanks() {
        let tabs = map_tabs(Layout {
            tabs: vec![
                tab(0, 1, "a", true, vec![pane("first", "/first", false)]),
                tab(1, 2, "b", false, Vec::new()),
            ],
        });

        assert_eq!(tabs[0].pane_title, "first");
        assert_eq!(tabs[1].pane_title, "");
        assert_eq!(tabs[1].pane_path, "");
    }

    #[test]
    fn an_unnamed_tab_takes_a_positional_label_and_key() {
        let tabs = map_tabs(Layout {
            tabs: vec![tab(2, 5, "   ", false, Vec::new())],
        });

        assert_eq!(tabs[0].key, "tab 3");
        assert_eq!(tabs[0].label, "tab 3");
        assert_eq!(tabs[0].pill_label(), "3 tab 3");
    }

    #[test]
    fn a_repeated_tab_name_keeps_a_unique_key_so_the_sheet_cannot_switch_to_the_wrong_card() {
        let tabs = map_tabs(Layout {
            tabs: vec![
                tab(0, 1, "zsh", false, Vec::new()),
                tab(1, 2, "zsh", false, Vec::new()),
                tab(2, 3, "zsh", false, Vec::new()),
            ],
        });

        let keys: Vec<&str> = tabs.iter().map(|tab| tab.key.as_str()).collect();
        assert_eq!(keys, ["zsh", "zsh 2", "zsh 3"]);
        assert_eq!(
            tabs[1].label, "zsh",
            "only the lookup key is disambiguated; the pill still reads the tab's own name"
        );
    }

    #[test]
    fn an_empty_layout_maps_to_no_tabs() {
        assert!(map_tabs(Layout { tabs: Vec::new() }).is_empty());
    }

    #[test]
    fn a_multi_pane_tabs_pane_list_carries_every_pane_field_for_field_in_wire_order() {
        let mut first = pane("htop", "/home/ed", true);
        first.id = 3;
        first.is_plugin = true;
        first.is_fullscreen = true;
        first.exited = true;
        first.is_floating = true;
        first.x = 1;
        first.y = 2;
        first.rows = 24;
        first.cols = 80;
        let mut second = pane("zsh", "/srv", false);
        second.id = 9;

        let tabs = map_tabs(Layout {
            tabs: vec![tab(0, 1, "dev", true, vec![first.clone(), second.clone()])],
        });

        assert_eq!(tabs[0].panes.len(), 2, "wire order, nothing dropped");
        let mapped_first = &tabs[0].panes[0];
        assert_eq!(mapped_first.id, first.id);
        assert_eq!(mapped_first.title, first.title);
        assert_eq!(mapped_first.cwd, first.cwd);
        assert_eq!(mapped_first.focused, first.is_focused);
        assert_eq!(mapped_first.floating, first.is_floating);
        assert_eq!(mapped_first.exited, first.exited);
        assert_eq!(mapped_first.is_plugin, first.is_plugin);
        assert_eq!(mapped_first.is_fullscreen, first.is_fullscreen);
        assert_eq!(mapped_first.x, first.x);
        assert_eq!(mapped_first.y, first.y);
        assert_eq!(mapped_first.rows, first.rows);
        assert_eq!(mapped_first.cols, first.cols);

        let mapped_second = &tabs[0].panes[1];
        assert_eq!(mapped_second.id, second.id);
        assert_eq!(mapped_second.title, second.title);
        assert!(
            !mapped_second.focused,
            "only the first pane above was marked focused"
        );
    }

    /// The fixture tab id [`tab`] marks as ringing.
    const BELL_TAB_ID: u32 = 7;

    #[test]
    fn a_layout_carries_each_tabs_bell_flag_rather_than_dropping_it() {
        let tabs = map_tabs(Layout {
            tabs: vec![
                tab(0, BELL_TAB_ID, "btop", false, Vec::new()),
                tab(1, 9, "devbox", true, Vec::new()),
            ],
        });
        assert!(tabs[0].has_bell);
        assert!(!tabs[1].has_bell);
    }

    #[test]
    fn a_paneless_tab_maps_to_an_empty_pane_list() {
        let tabs = map_tabs(Layout {
            tabs: vec![tab(0, 1, "dev", true, Vec::new())],
        });
        assert!(tabs[0].panes.is_empty());
    }

    // -- what the binding puts on the wire ------------------------------------

    #[test]
    fn binding_rides_on_every_request_that_has_a_field_for_it() {
        let repo = GrpcTerminalRepository::new(Arc::new(ConnectionController::detached()));
        assert_eq!(repo.session_ref(), None, "unbound addresses nothing");

        repo.bind(Some("herdr:work".to_string()), Some("conn-7".to_string()));
        assert_eq!(
            repo.session_ref(),
            Some(SessionRef {
                session: "herdr:work".to_string(),
                connection_id: "conn-7".to_string(),
                space_id: String::new(),
            })
        );
        assert_eq!(
            repo.addressed(),
            Ok(("herdr:work".to_string(), "conn-7".to_string()))
        );
    }

    #[test]
    fn a_binding_with_no_connection_id_yet_sends_the_wires_own_fallback_routing() {
        let repo = repo(Some("zellij:dev"));
        assert_eq!(
            repo.session_ref().map(|request| request.connection_id),
            Some(String::new()),
            "empty is the wire's documented legacy/fallback routing"
        );
    }

    #[test]
    fn rebinding_replaces_the_id_a_re_attached_stream_superseded() {
        let repo = repo(Some("zellij:dev"));
        repo.bind(Some("zellij:dev".to_string()), Some("conn-1".to_string()));
        repo.bind(Some("zellij:dev".to_string()), Some("conn-2".to_string()));

        assert_eq!(repo.addressed().map(|(_, id)| id), Ok("conn-2".to_string()));
    }

    // -- the two calls that never reach the wire ------------------------------

    #[test]
    fn tab_lines_answers_empty_because_the_emulator_owns_a_live_sessions_content() {
        // Disconnected on purpose: this must not dial at all, so the "not
        // connected" failure every wire-bound call answers is exactly what it
        // must *not* produce.
        let repo = repo(Some("zellij:dev"));
        assert_eq!(
            block_on(repo.tab_lines("devbox".to_string())),
            Ok(Vec::new())
        );
    }

    #[test]
    fn detach_validates_its_id_and_never_dials() {
        let repo = repo(Some("zellij:dev"));
        assert!(block_on(repo.detach_session("zellij:dev".to_string())).is_ok());
        assert!(matches!(
            block_on(repo.detach_session("   ".to_string())),
            Err(AppFailure::Internal(_))
        ));
    }

    // -- the unbound and disconnected paths -----------------------------------

    #[test]
    fn an_unbound_repository_lists_nothing_rather_than_failing_a_bare_terminal_route() {
        let repo = repo(None);
        assert_eq!(block_on(repo.list_spaces()), Ok(Vec::new()));
        assert_eq!(block_on(repo.list_tabs()), Ok(Vec::new()));
    }

    #[test]
    fn an_unbound_switch_is_an_internal_failure_rather_than_a_retryable_one() {
        let repo = repo(None);
        assert!(matches!(
            block_on(repo.switch_space("ws-1".to_string())),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.go_to_tab(3)),
            Err(AppFailure::Internal(_))
        ));
    }

    #[test]
    fn every_wire_bound_method_fails_cleanly_when_disconnected() {
        let repo = repo(Some("zellij:dev"));

        assert!(matches!(
            block_on(repo.list_spaces()),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.list_tabs()),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.switch_space("ws-1".to_string())),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.go_to_tab(3)),
            Err(AppFailure::Network(_))
        ));
    }

    #[test]
    fn every_workspace_verb_fails_cleanly_when_disconnected() {
        let repo = repo(Some("zellij:dev"));

        assert!(matches!(
            block_on(repo.create_space("space".to_string())),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.rename_space("ws-1".to_string(), "space".to_string())),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.new_tab("name".to_string())),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.rename_tab(1, "name".to_string())),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.new_pane(false)),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.rename_pane(1, false, "name".to_string())),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.focus_pane(1, false)),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.scroll_pane(1, false, ScrollStep::Up)),
            Err(AppFailure::Network(_))
        ));
    }

    #[test]
    fn an_unbound_workspace_verb_is_an_internal_failure_rather_than_a_retryable_one() {
        let repo = repo(None);
        assert!(matches!(
            block_on(repo.create_space("space".to_string())),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.focus_pane(1, false)),
            Err(AppFailure::Internal(_))
        ));
    }

    // -- request construction --------------------------------------------------

    #[test]
    fn create_space_request_carries_session_label_and_connection_id() {
        let request = create_space_request(
            "zellij:dev".to_string(),
            "conn-1".to_string(),
            "ws".to_string(),
        );
        assert_eq!(request.session, "zellij:dev");
        assert_eq!(request.label, "ws");
        assert_eq!(
            request.connection_id, "conn-1",
            "CreateSpaceReq carries connection_id"
        );
    }

    #[test]
    fn rename_space_request_carries_every_field_incl_connection_id() {
        let request = rename_space_request(
            "zellij:dev".to_string(),
            "conn-1".to_string(),
            "ws-1".to_string(),
            "renamed".to_string(),
        );
        assert_eq!(request.session, "zellij:dev");
        assert_eq!(request.space_id, "ws-1");
        assert_eq!(request.label, "renamed");
        assert_eq!(request.connection_id, "conn-1");
    }

    #[test]
    fn new_tab_request_carries_the_session_and_name() {
        let request = new_tab_request("zellij:dev".to_string(), "build".to_string());
        assert_eq!(request.session, "zellij:dev");
        assert_eq!(request.tab_name, "build");
    }

    #[test]
    fn new_tab_request_carries_an_empty_name_when_the_dialog_confirms_blank() {
        let request = new_tab_request("zellij:dev".to_string(), String::new());
        assert_eq!(request.session, "zellij:dev");
        assert_eq!(
            request.tab_name, "",
            "a blank confirm is this create's own valid default, not a \
             disabled state — NewTabReq has no connection_id field either way"
        );
    }

    #[test]
    fn rename_tab_request_carries_no_connection_id_field() {
        let request = rename_tab_request("zellij:dev".to_string(), 7, "renamed".to_string());
        assert_eq!(request.session, "zellij:dev");
        assert_eq!(request.tab_id, 7);
        assert_eq!(request.name, "renamed");
    }

    #[test]
    fn new_pane_request_carries_the_floating_flag_unnamed() {
        let floating = new_pane_request("zellij:dev".to_string(), true);
        assert_eq!(floating.session, "zellij:dev");
        assert!(floating.floating);
        assert_eq!(floating.pane_name, "");

        let tiled = new_pane_request("zellij:dev".to_string(), false);
        assert!(!tiled.floating);
    }

    #[test]
    fn pane_target_carries_every_field_incl_connection_id() {
        let target = pane_target("zellij:dev".to_string(), "conn-1".to_string(), 3, true);
        assert_eq!(target.session, "zellij:dev");
        assert_eq!(target.pane_id, 3);
        assert!(target.is_plugin);
        assert_eq!(
            target.connection_id, "conn-1",
            "PaneTarget carries connection_id"
        );
    }

    #[test]
    fn rename_pane_request_wraps_a_pane_target_and_carries_the_new_name() {
        let request = rename_pane_request(
            "zellij:dev".to_string(),
            "conn-1".to_string(),
            3,
            false,
            "renamed".to_string(),
        );
        let target = request.target.expect("RenamePaneReq.target is populated");
        assert_eq!(target.session, "zellij:dev");
        assert_eq!(target.pane_id, 3);
        assert!(!target.is_plugin);
        assert_eq!(target.connection_id, "conn-1");
        assert_eq!(request.name, "renamed");
    }

    #[test]
    fn scroll_pane_request_wraps_a_pane_target_and_maps_the_step() {
        let up = scroll_pane_request(
            "zellij:dev".to_string(),
            "conn-1".to_string(),
            3,
            false,
            ScrollStep::Up,
        );
        let target = up.target.expect("ScrollReq.target is populated");
        assert_eq!(target.pane_id, 3);
        assert_eq!(target.connection_id, "conn-1");
        assert_eq!(up.direction, ScrollDirection::Up as i32);

        let down = scroll_pane_request(
            "zellij:dev".to_string(),
            "conn-1".to_string(),
            3,
            false,
            ScrollStep::Down,
        );
        assert_eq!(down.direction, ScrollDirection::Down as i32);
    }

    #[test]
    fn map_scroll_step_covers_both_wheel_notches() {
        assert_eq!(map_scroll_step(ScrollStep::Up), ScrollDirection::Up);
        assert_eq!(map_scroll_step(ScrollStep::Down), ScrollDirection::Down);
    }

    #[tokio::test]
    async fn toggle_pane_fullscreen_sends_the_bound_target_without_a_floating_hint() {
        let fixture = MuxrdFixture::start().await;
        let repo = fixture_repo(&fixture).await;
        repo.bind(
            Some(testing::SESSION_NAMES[0].to_string()),
            Some("conn-9".to_string()),
        );

        repo.toggle_pane_fullscreen(7, true)
            .await
            .expect("the fixture acks");

        let calls = fixture.actions().toggle_pane_fullscreen_calls();
        assert_eq!(calls.len(), 1);
        let target = calls[0].target.clone().expect("a target");
        assert_eq!(target.session, testing::SESSION_NAMES[0]);
        assert_eq!(target.connection_id, "conn-9");
        assert_eq!(target.pane_id, 7);
        assert!(target.is_plugin);
        assert!(!calls[0].has_floating_hint);
    }

    #[tokio::test]
    async fn toggle_pane_fullscreen_surfaces_a_refused_ack_as_an_error() {
        let fixture = MuxrdFixture::start().await;
        let repo = fixture_repo(&fixture).await;
        fixture
            .actions()
            .refuse_toggle_pane_fullscreen("read-only session");

        let failure = repo
            .toggle_pane_fullscreen(1, false)
            .await
            .expect_err("a refused ack fails");
        assert!(failure.to_string().contains("read-only session"));
        assert_eq!(fixture.actions().toggle_pane_fullscreen_calls().len(), 1);
    }

    // -- ack + status mapping -------------------------------------------------

    #[test]
    fn a_refused_ack_fails_through_the_same_net_error_path_every_switch_uses() {
        let refused = ActionAck {
            ok: false,
            error: "read-only session".to_string(),
            info: String::new(),
        };
        let failure = error::ack_result(&refused).expect_err("a refused action fails");
        assert!(matches!(failure, AppFailure::Network(_)));
        assert!(failure.to_string().contains("read-only session"));
    }

    #[tokio::test]
    async fn an_unauthenticated_status_logs_the_session_out_through_the_shared_hook() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;
        let connection = ConnectionController::with_live_client_for_tests(client.clone());
        assert!(connection.connected.get_untracked());

        let repo = GrpcTerminalRepository::new(Arc::clone(&connection));
        let failure = repo.map_status(&client, Status::unauthenticated("session token expired"));

        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(
            !connection.connected.get_untracked(),
            "the session-expired hook tears the connection down"
        );
    }

    #[tokio::test]
    async fn an_unauthenticated_status_from_a_replaced_client_leaves_the_connection_alone() {
        let fixture = MuxrdFixture::start().await;
        let connection =
            ConnectionController::with_live_client_for_tests(fixture.connect_pinned().await);
        let stale = fixture.connect_pinned().await;

        let repo = GrpcTerminalRepository::new(Arc::clone(&connection));
        let failure = repo.map_status(&stale, Status::unauthenticated("session token expired"));

        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(connection.connected.get_untracked());
    }

    #[tokio::test]
    async fn a_non_auth_status_never_touches_the_connection() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;
        let connection = ConnectionController::with_live_client_for_tests(client.clone());
        let repo = GrpcTerminalRepository::new(Arc::clone(&connection));

        let failure = repo.map_status(&client, Status::unavailable("connection refused"));

        assert!(matches!(failure, AppFailure::Network(_)));
        assert!(
            connection.connected.get_untracked(),
            "only an auth failure logs out"
        );
    }
}
