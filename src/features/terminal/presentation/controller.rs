//! The `/terminal` view model. Tabs and panes have no server push, so every refresh races: a
//! generation counter commits only the newest fetch, and single-flight claims guard the poll,
//! creates and every move. The feed writes no signal; `mirror_link` turns link status into one.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use clean_signals::{ControllerCore, RunOptions};
use clean_signals_frust::use_interval;
use frust::authoring::text::FontFamily;
use frust::{Get, GetUntracked, RwSignal, Set, With, WithUntracked, spawn_local};

use crate::agents::{AgentStatus, agent_sort_key};
use crate::connection::{CAP_AGENTS_STREAM, ConnectionController};
use crate::failure::AppFailure;
use crate::features::terminal::data::feeds::demo_feed::DemoFeed;
use crate::features::terminal::data::feeds::grpc_feed::{GrpcTerminalFeed, ResumeSlot};
use crate::features::terminal::domain::entities::{
    AgentEntry, AgentEventEntry, PaneEntry, RenameRequest, ScrollStep, SettingsCursorStyle, Space,
    TabEntry, TermLine, Workspace,
};
use crate::features::terminal::domain::feed::{LinkStatus, TerminalFeed};
use crate::features::terminal::domain::repositories::{
    AgentSink, PreferencesRepository, SessionsRepository, TerminalRepository, WatchEnd, WatchHandle,
};
use crate::features::terminal::domain::use_cases::{
    DetachSession, ListSpaceTabs, LoadWorkspace, RenameAttachedSession, SwitchTab, WorkspaceQuery,
};
use crate::fonts;
use crate::net::client::MuxrClient;

use super::term;

pub struct TerminalController {
    core: ControllerCore<AppFailure>,
    load: LoadWorkspace,
    switch: SwitchTab,
    rename: RenameAttachedSession,
    detach: DetachSession,
    terminal: Arc<dyn TerminalRepository + Send + Sync>,
    session_id: Option<String>,
    pub spaces: RwSignal<Vec<Space>>,
    pub tabs: RwSignal<Vec<TabEntry>>,
    pub active_tab: RwSignal<String>,
    list_space_tabs: ListSpaceTabs,
    /// Id-keyed, never index-keyed: the poll may republish [`tabs`](Self::tabs) mid-open.
    pub expanded_space: RwSignal<Option<String>>,
    pub expanded_tab: RwSignal<Option<String>>,
    /// A browsed non-selected space's tabs; never a copy of the selected space's [`tabs`](Self::tabs).
    pub browsed_space_tabs: RwSignal<Option<(String, Vec<TabEntry>)>>,
    pub lines: RwSignal<Vec<TermLine>>,
    pub agents: RwSignal<Vec<AgentEntry>>,
    /// Ahead of [`agents`](Self::agents) while a move claim holds the published rows still.
    agent_rows: Mutex<Vec<AgentEntry>>,
    agent_watch: Mutex<Option<WatchHandle>>,
    /// Only a live-to-down transition flags [`agents_stale`](Self::agents_stale), never a stream still opening.
    agent_watch_was_live: AtomicBool,
    /// How to reopen an ended subscription, and from when; `None` once it may never reopen.
    agent_watch_reopen: Mutex<Option<WatchReopen>>,
    agent_watch_hooked: AtomicBool,
    agents_listed_at: Mutex<Option<Instant>>,
    /// Panes the stream touched during an in-flight relist, so its answer merges rather than overwrites.
    agents_touched: Mutex<Option<HashMap<u32, Touch>>>,
    agents_stale_raises: AtomicU64,
    /// The last newest-generation `list_agents` failed; the rows stay published and [`has_blocked_agent`](Self::has_blocked_agent) mutes.
    pub agents_stale: RwSignal<bool>,
    pub session_name: RwSignal<Option<String>>,
    pub key_bar_visible: RwSignal<bool>,
    pub fn_row_visible: RwSignal<bool>,
    pub loaded: RwSignal<bool>,
    pub fullscreen: RwSignal<bool>,
    pub reveal_epoch: RwSignal<u64>,
    /// Atomic, so a bump from an owner-less press handler never reads what it writes.
    next_epoch: AtomicU64,
    /// Entered once per mount; there is no path back.
    pub live: RwSignal<bool>,
    live_attached: AtomicBool,
    pub link: RwSignal<Option<LinkStatus>>,
    /// A `Mutex`, not a `Cell`: this type is `Send + Sync`.
    bound_connection_id: Mutex<String>,
    /// Seeded with the shipped default so the first live frame is right before the prefs read lands.
    pub cursor_style: RwSignal<term::CursorStyle>,
    pub font_size: RwSignal<f32>,
    pub font_family: RwSignal<FontFamily>,
    /// Resolved once by the factory, never per poll tick.
    preferences: Arc<dyn PreferencesRepository + Send + Sync>,
    appearance_writes: AtomicU64,
    /// Bumped before a round trip, re-checked before commit, so a slower fetch never rolls back a newer one.
    layout_generation: AtomicU64,
    agents_generation: AtomicU64,
    /// Scoped to the poll only, so a slow link never stacks round trips.
    layout_poll_in_flight: AtomicBool,
    /// A dropped press stays dropped.
    create_in_flight: AtomicBool,
    /// One slot for every move, the sheet's commit and the space switches alike: two racing moves
    /// would leave the connection on one space with another space's tab world published.
    move_in_flight: AtomicBool,
    /// Its own slot: a pinch neither waits on nor blocks a create or move.
    fullscreen_toggle_in_flight: AtomicBool,
    /// Non-zero while a focus-moving verb is in flight; [`apply_attach_target`](Self::apply_attach_target) defers meanwhile.
    focus_verbs_in_flight: AtomicUsize,
    attach_settled: AtomicBool,
    /// A fetch opened at or before this generation had no connection to route by.
    settle_generation: AtomicU64,
    published_generation: AtomicU64,
    layout_writes: AtomicU64,
    /// Written only by user-driven moves.
    resume: Arc<ResumeSlot>,
    /// `Relaxed`: atomics for the `Send + Sync` bound on the UI executor, not for concurrency.
    docked: AtomicBool,
    attach_target: Mutex<Option<AttachTarget>>,
}

/// Applied at most once, against a tab list published after the link could route. Only a landed
/// focus-moving verb supersedes it; a refused claim, a failed move and a space create keep it,
/// and it waits out any such verb still in flight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachTarget {
    /// `None` or the selected space resolves in place; any other is peeked, then switched to first.
    pub space_id: Option<String>,
    /// `TabMsg.position`, never the tab id or a title, counted in the target space's own layout.
    pub tab_position: Option<u32>,
    /// `PaneMsg.id` of a terminal pane.
    pub pane_id: Option<u32>,
}

/// Transcribed from settings' `Preferences::defaults()`.
const DEFAULT_LIVE_FONT_SIZE: f32 = 12.0;

/// `PreferencesRepository` has no push seam; `sessions`' own 3s cadence.
const APPEARANCE_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// The wire has no layout-changed signal; no tighter, since RPCs share the attach stream's HTTP/2 connection.
const LAYOUT_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// A full relist under a live agents stream, as a safety net.
const AGENTS_SAFETY_RELIST: Duration = Duration::from_secs(30);

/// So a server ending every stream at once (zellij) costs one open per window.
const AGENT_WATCH_RETRY: Duration = Duration::from_secs(30);

/// How many streamed agent events may wait for the UI thread; coalescing keeps it far below.
const AGENT_INBOX_CAPACITY: usize = 64;

static AGENTS_CLOCK_AHEAD_MS: AtomicU64 = AtomicU64::new(0);

/// `pub`, not `#[cfg(test)]`: `tests/*.rs` link the normal build.
pub fn advance_agents_clock_for_tests(by: Duration) {
    AGENTS_CLOCK_AHEAD_MS.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
}

fn agents_now() -> Instant {
    Instant::now() + Duration::from_millis(AGENTS_CLOCK_AHEAD_MS.load(Ordering::SeqCst))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Touch {
    Changed,
    /// The held row's `change_seq`, `u64::MAX` when none was held, so a relist never resurrects it.
    Removed(u64),
}

struct WatchReopen {
    controller: Weak<TerminalController>,
    connection: Weak<ConnectionController>,
    opened_at: Instant,
    not_before: Option<Instant>,
}

/// A snapshot supersedes everything queued before it and a later event for a pane an earlier
/// one; past [`AGENT_INBOX_CAPACITY`] the oldest drops and the next tick relists.
#[derive(Default)]
struct AgentInbox {
    queue: Mutex<VecDeque<AgentEventEntry>>,
    wake: tokio::sync::Notify,
    closed: AtomicBool,
    overflowed: AtomicBool,
}

impl AgentInbox {
    fn push(&self, event: AgentEventEntry) {
        {
            let mut queue = lock(&self.queue);
            match event_pane(&event) {
                None => queue.clear(),
                Some(pane) => queue.retain(|queued| event_pane(queued) != Some(pane)),
            }
            if queue.len() >= AGENT_INBOX_CAPACITY {
                queue.pop_front();
                self.overflowed.store(true, Ordering::SeqCst);
            }
            queue.push_back(event);
        }
        self.wake.notify_one();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// `None` once closed and drained.
    async fn next_batch(&self) -> Option<Vec<AgentEventEntry>> {
        loop {
            let closed = self.closed.load(Ordering::SeqCst);
            let batch: Vec<AgentEventEntry> = lock(&self.queue).drain(..).collect();
            if !batch.is_empty() {
                return Some(batch);
            }
            if closed {
                return None;
            }
            self.wake.notified().await;
        }
    }
}

fn event_pane(event: &AgentEventEntry) -> Option<u32> {
    match event {
        AgentEventEntry::Snapshot(_) => None,
        AgentEventEntry::Changed(agent) => Some(agent.pane_id),
        AgentEventEntry::Removed(pane_id) => Some(*pane_id),
    }
}

/// Dropping it, when the stream is over, closes the inbox.
struct InboxSender(Arc<AgentInbox>);

impl Drop for InboxSender {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// The relay acks on enqueue, not apply; 350ms is the original client's settle.
const CREATE_SETTLE: Duration = Duration::from_millis(350);

const NO_SETTLE_OVERRIDE: u64 = u64::MAX;

static CREATE_SETTLE_OVERRIDE_MS: AtomicU64 = AtomicU64::new(NO_SETTLE_OVERRIDE);

/// `pub`, not `#[cfg(test)]`: `tests/*.rs` link the normal build.
pub fn set_create_settle_for_tests(settle: Option<Duration>) {
    let value = settle.map_or(NO_SETTLE_OVERRIDE, |settle| settle.as_millis() as u64);
    CREATE_SETTLE_OVERRIDE_MS.store(value, Ordering::Relaxed);
}

fn create_settle() -> Duration {
    match CREATE_SETTLE_OVERRIDE_MS.load(Ordering::Relaxed) {
        NO_SETTLE_OVERRIDE => CREATE_SETTLE,
        millis => Duration::from_millis(millis),
    }
}

/// A full load re-seeds the keys outright; a partial reload only heals a key that names no tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seed {
    Fresh,
    Heal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Report {
    Toast,
    /// The poll runs for the mount's life, so a down link would toast on a cadence.
    Silent,
    /// The attach-settle edge: a transient reconnect failure would toast though the attach succeeds.
    Quiet,
}

static FAIL_NEXT_TAB_LIST: AtomicBool = AtomicBool::new(false);

/// `pub`, not `#[cfg(test)]`: `tests/*.rs` link the normal build.
pub fn fail_next_tab_list_for_tests() {
    FAIL_NEXT_TAB_LIST.store(true, Ordering::SeqCst);
}

fn map_cursor_style(style: SettingsCursorStyle) -> term::CursorStyle {
    match style {
        SettingsCursorStyle::Block => term::CursorStyle::Block,
        SettingsCursorStyle::Underline => term::CursorStyle::Underline,
        SettingsCursorStyle::Bar => term::CursorStyle::Bar,
    }
}

impl TerminalController {
    pub fn new(
        terminal: Arc<dyn TerminalRepository + Send + Sync>,
        sessions: Arc<dyn SessionsRepository + Send + Sync>,
        preferences: Arc<dyn PreferencesRepository + Send + Sync>,
        session_id: Option<String>,
    ) -> Self {
        // The connection id only exists once a stream has minted one.
        terminal.bind(session_id.clone(), None);
        Self {
            core: ControllerCore::new(),
            load: LoadWorkspace::new(Arc::clone(&terminal), Arc::clone(&sessions)),
            switch: SwitchTab::new(Arc::clone(&terminal)),
            rename: RenameAttachedSession::new(sessions),
            detach: DetachSession::new(Arc::clone(&terminal)),
            list_space_tabs: ListSpaceTabs::new(Arc::clone(&terminal)),
            terminal,
            session_id,
            spaces: RwSignal::new(Vec::new()),
            tabs: RwSignal::new(Vec::new()),
            active_tab: RwSignal::new(String::new()),
            expanded_space: RwSignal::new(None),
            expanded_tab: RwSignal::new(None),
            browsed_space_tabs: RwSignal::new(None),
            lines: RwSignal::new(Vec::new()),
            agents: RwSignal::new(Vec::new()),
            agents_stale: RwSignal::new(false),
            agent_rows: Mutex::new(Vec::new()),
            agent_watch: Mutex::new(None),
            agent_watch_was_live: AtomicBool::new(false),
            agent_watch_reopen: Mutex::new(None),
            agent_watch_hooked: AtomicBool::new(false),
            agents_listed_at: Mutex::new(None),
            agents_touched: Mutex::new(None),
            agents_stale_raises: AtomicU64::new(0),
            session_name: RwSignal::new(None),
            key_bar_visible: RwSignal::new(true),
            fn_row_visible: RwSignal::new(false),
            loaded: RwSignal::new(false),
            fullscreen: RwSignal::new(false),
            reveal_epoch: RwSignal::new(0),
            next_epoch: AtomicU64::new(1),
            live: RwSignal::new(false),
            live_attached: AtomicBool::new(false),
            link: RwSignal::new(None),
            bound_connection_id: Mutex::new(String::new()),
            cursor_style: RwSignal::new(term::CursorStyle::Block),
            font_size: RwSignal::new(DEFAULT_LIVE_FONT_SIZE),
            font_family: RwSignal::new(fonts::default_terminal_family()),
            preferences,
            appearance_writes: AtomicU64::new(0),
            layout_generation: AtomicU64::new(0),
            agents_generation: AtomicU64::new(0),
            layout_poll_in_flight: AtomicBool::new(false),
            create_in_flight: AtomicBool::new(false),
            move_in_flight: AtomicBool::new(false),
            fullscreen_toggle_in_flight: AtomicBool::new(false),
            focus_verbs_in_flight: AtomicUsize::new(0),
            attach_settled: AtomicBool::new(false),
            settle_generation: AtomicU64::new(0),
            published_generation: AtomicU64::new(0),
            layout_writes: AtomicU64::new(0),
            resume: Arc::new(ResumeSlot::default()),
            docked: AtomicBool::new(false),
            attach_target: Mutex::new(None),
        }
    }

    /// Named, since the [`AsRef`] impl's bare call is ambiguous.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    pub fn session_id(&self) -> Option<String> {
        self.session_id.clone()
    }

    /// A second handle onto the same slot, so a later move still rides the next reconnect.
    pub fn resume_slot(&self) -> Arc<ResumeSlot> {
        Arc::clone(&self.resume)
    }

    /// Only the tabs go through the staleness guard; nothing else reads that generation.
    pub async fn load(&self) {
        let generation = self.begin_layout_fetch();
        let query = WorkspaceQuery {
            session_id: self.session_id.clone(),
            with_spaces: self.has_spaces(),
        };
        let Ok(workspace) = self
            .core
            .run(&self.load, query, RunOptions::default())
            .await
        else {
            return;
        };
        let Workspace {
            spaces,
            tabs,
            lines,
            session_name,
        } = workspace;
        if !self.signals_alive() {
            return;
        }
        self.spaces.set(spaces);
        self.publish_if_newest(generation, tabs, Seed::Fresh);
        self.lines.set(lines);
        self.session_name.set(session_name);
        self.loaded.set(true);
        self.replay_reveal();
        if self.is_docked() {
            self.settle_rest();
        }
        self.apply_attach_target().await;
        self.refresh_agents().await;
    }

    pub fn set_attach_target(&self, tab_position: Option<u32>, pane_id: Option<u32>) {
        self.set_attach_target_in_space(None, tab_position, pane_id);
    }

    /// A link naming a space alone records nothing: there is nowhere in it to land.
    pub fn set_attach_target_in_space(
        &self,
        space_id: Option<String>,
        tab_position: Option<u32>,
        pane_id: Option<u32>,
    ) {
        let target = (tab_position.is_some() || pane_id.is_some()).then_some(AttachTarget {
            space_id,
            tab_position,
            pane_id,
        });
        *self
            .attach_target
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = target;
    }

    #[doc(hidden)]
    pub fn pending_attach_target_for_tests(&self) -> Option<AttachTarget> {
        self.attach_target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn supersede_attach_target(&self) {
        self.set_attach_target(None, None);
    }

    /// Whichever caller finds the published list ready applies it. The move claim is taken before
    /// the target, so a claim held elsewhere leaves it for the next publish; an unknown tab or pane
    /// drops it silently.
    pub async fn apply_attach_target(&self) {
        if !self.attach_target_ready() || self.focus_verbs_in_flight.load(Ordering::SeqCst) != 0 {
            return;
        }
        let Some(_claim) = self.claim_move() else {
            return;
        };
        let Some(target) = self
            .attach_target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        if let Some(space_id) = target.space_id.clone()
            && self.selected_space_id().as_deref() != Some(space_id.as_str())
        {
            if self.has_spaces() {
                self.apply_attach_target_in_space(space_id, &target).await;
            } else {
                log::debug!("attach target: space {space_id} on a backend without spaces, dropped");
            }
            return;
        }
        let (Some(tabs), Some(active)) = (
            self.tabs.try_get_untracked(),
            self.active_tab.try_get_untracked(),
        ) else {
            return;
        };
        let Some((tab, pane_id)) = resolve_attach_target(&tabs, &active, &target) else {
            return;
        };
        let landed = match pane_id {
            Some(pane_id) => {
                self.commit_pane_unclaimed(tab.key.clone(), pane_id, false)
                    .await
            }
            None if tab.key != active => self.switch_tab(tab.key.clone()).await,
            None => true,
        };
        if landed && self.is_docked() {
            self.settle_rest();
        }
    }

    /// Fail-closed: a failed peek, or a tab or pane the peeked layout lacks, sends nothing.
    async fn apply_attach_target_in_space(&self, space_id: String, target: &AttachTarget) {
        let Ok(tabs) = self
            .core
            .run(
                &self.list_space_tabs,
                space_id.clone(),
                RunOptions::default(),
            )
            .await
        else {
            return;
        };
        if !self.signals_alive() {
            return;
        }
        let active = tabs
            .iter()
            .find(|tab| tab.active)
            .or_else(|| tabs.first())
            .map(|tab| tab.key.clone())
            .unwrap_or_default();
        let Some((tab, pane_id)) = resolve_attach_target(&tabs, &active, target) else {
            return;
        };
        let (tab_key, pane) = (tab.key.clone(), pane_id.map(|id| (id, false)));
        let landed = self.land_in_space(space_id, tabs, tab_key, pane).await;
        if landed && self.is_docked() {
            self.settle_rest();
        }
    }

    /// On a live mount only a fetch opened after the handshake settled counts, since an earlier
    /// one had no connection to route by.
    fn attach_target_ready(&self) -> bool {
        let floor = if self.live_attached.load(Ordering::Relaxed) {
            if !self.attach_settled.load(Ordering::SeqCst) {
                return false;
            }
            self.settle_generation.load(Ordering::SeqCst)
        } else {
            0
        };
        self.published_generation.load(Ordering::SeqCst) > floor
            && self.loaded.try_get_untracked() == Some(true)
            && self.tabs.try_with_untracked(|tabs| !tabs.is_empty()) == Some(true)
    }

    /// Never `switch_space`: browsing must not move this connection's selection. The answer
    /// publishes only if [`expanded_space`](Self::expanded_space) still names `id`; a failure
    /// toasts once and collapses the row.
    pub async fn expand_space(&self, id: String) {
        // Reached a pump after the press; the screen may be gone and a disposed read panics.
        let Some(expanded) = self.expanded_space.try_get_untracked() else {
            return;
        };
        if expanded.as_deref() != Some(id.as_str()) {
            self.expanded_tab.set(None);
        }
        self.expanded_space.set(Some(id.clone()));
        if self.selected_space_id().as_deref() == Some(id.as_str()) {
            return;
        }
        match self
            .core
            .run(&self.list_space_tabs, id.clone(), RunOptions::default())
            .await
        {
            Ok(tabs) => {
                if self.expanded_space.try_get_untracked() == Some(Some(id.clone())) {
                    self.browsed_space_tabs.set(Some((id, tabs)));
                }
            }
            Err(_) => {
                if self.expanded_space.try_get_untracked() == Some(Some(id)) {
                    self.expanded_space.set(None);
                }
            }
        }
    }

    pub fn collapse_space(&self) {
        self.expanded_space.set(None);
        self.expanded_tab.set(None);
    }

    pub fn expand_tab(&self, key: String) {
        self.expanded_tab.set(Some(key));
    }

    pub fn collapse_tab(&self) {
        self.expanded_tab.set(None);
    }

    pub fn seed_expansion(&self) {
        if self.is_herdr_session() {
            self.expanded_space.set(self.selected_space_id());
        }
        self.expanded_tab.set(Some(self.active_tab.get_untracked()));
    }

    pub fn set_docked(&self, docked: bool) -> bool {
        self.docked.swap(docked, Ordering::Relaxed) != docked
    }

    pub fn is_docked(&self) -> bool {
        self.docked.load(Ordering::Relaxed)
    }

    /// A landed load, a landed commit and a closed foreign browse all rest here; a no-op on a disposed mount.
    pub fn settle_rest(&self) {
        let Some(tabs) = self.tabs.try_get_untracked() else {
            return;
        };
        let key = rest_tab_key(&tabs).or_else(|| self.active_tab.try_get_untracked());
        if self.is_herdr_session() {
            self.expanded_space.set(self.selected_space_id());
        }
        self.expanded_tab.set(key);
        self.browsed_space_tabs.set(None);
    }

    pub fn close_space(&self, id: &str) {
        if self.is_docked() && self.selected_space_id().as_deref() != Some(id) {
            self.settle_rest();
        } else {
            self.collapse_space();
        }
    }

    pub fn reset_browse(&self) {
        self.expanded_space.set(None);
        self.expanded_tab.set(None);
        self.browsed_space_tabs.set(None);
    }

    /// The stream stays open: herdr re-points this connection's relay in place.
    pub async fn switch_tab(&self, key: String) -> bool {
        let Some(tab_id) = self.tab_id_for(&key) else {
            return false;
        };
        // Focus first: a refused switch must not leave the card on a tab the session is not on.
        if !self.tell_move(self.terminal.go_to_tab(tab_id)).await {
            return false;
        }
        let Ok(lines) = self
            .core
            .run(&self.switch, key.clone(), RunOptions::default())
            .await
        else {
            return false;
        };
        self.active_tab.set(key.clone());
        self.tabs.update_active(&key);
        self.lines.set(lines);
        self.replay_reveal();
        true
    }

    /// Tab move first (a pane only focuses inside the focused tab), then pane focus. A tap in flight
    /// is dropped, not queued. The badge write is not fenced: a same-space commit leaves the tab
    /// world alone.
    pub async fn commit_pane(&self, tab_key: String, pane_id: u32, is_plugin: bool) -> bool {
        let Some(_claim) = self.claim_user_move() else {
            return false;
        };
        self.commit_pane_unclaimed(tab_key, pane_id, is_plugin)
            .await
    }

    /// Teardown-safe: every read on this path is `try_*`, since the tap resumes a pump (or a
    /// switch) later; a commit whose screen is gone answers `false`.
    async fn commit_pane_unclaimed(&self, tab_key: String, pane_id: u32, is_plugin: bool) -> bool {
        let Some(active) = self.active_tab.try_get_untracked() else {
            return false;
        };
        if tab_key != active && !self.switch_tab(tab_key.clone()).await {
            return false;
        }
        if !self
            .tell_move(self.terminal.focus_pane(pane_id, is_plugin))
            .await
        {
            return false;
        }
        self.focus_pane_locally(&tab_key, pane_id, is_plugin);
        if self.is_herdr_session() {
            self.remember_resume_pane(&tab_key, pane_id);
        }
        // No reload: one could land before the enqueue-ack applies and revert the badge.
        true
    }

    /// Fail-closed on a lost cache: the browsed tabs are read before the first await, and with none
    /// cached for `space_id` nothing is sent. The selection follows the switch, since the tree keys
    /// its per-space tab world off [`spaces`](Self::spaces).
    pub async fn commit_pane_in_space(
        &self,
        space_id: String,
        tab_key: String,
        pane_id: u32,
        is_plugin: bool,
    ) -> bool {
        if self.selected_space_id().as_deref() == Some(space_id.as_str()) {
            return self.commit_pane(tab_key, pane_id, is_plugin).await;
        }
        let Some(cached_tabs) = self.browsed_tabs_for(&space_id) else {
            return false;
        };
        self.commit_pane_in_space_with(space_id, cached_tabs, tab_key, pane_id, is_plugin)
            .await
    }

    async fn commit_pane_in_space_with(
        &self,
        space_id: String,
        cached_tabs: Vec<TabEntry>,
        tab_key: String,
        pane_id: u32,
        is_plugin: bool,
    ) -> bool {
        let Some(_claim) = self.claim_user_move() else {
            return false;
        };
        self.land_in_space(space_id, cached_tabs, tab_key, Some((pane_id, is_plugin)))
            .await
    }

    async fn land_in_space(
        &self,
        space_id: String,
        tabs: Vec<TabEntry>,
        tab_key: String,
        pane: Option<(u32, bool)>,
    ) -> bool {
        if !self
            .tell_move(self.terminal.switch_space(space_id.clone()))
            .await
        {
            return false;
        }
        if self.is_herdr_session() {
            self.resume.remember_space(space_id.clone());
        }
        self.select_space_locally(&space_id);
        self.promote_browsed_tabs(tabs);
        let landed = match pane {
            Some((pane_id, is_plugin)) => {
                self.commit_pane_unclaimed(tab_key, pane_id, is_plugin)
                    .await
            }
            None => {
                let Some(active) = self.active_tab.try_get_untracked() else {
                    return false;
                };
                tab_key == active || self.switch_tab(tab_key).await
            }
        };
        // Behind the pane steps: a user waiting on focus never waits on this read.
        self.publish_space_switch(&space_id).await;
        landed
    }

    fn browsed_tabs_for(&self, space_id: &str) -> Option<Vec<TabEntry>> {
        match self.browsed_space_tabs.try_get_untracked()? {
            Some((cached_space, cached_tabs)) if cached_space == space_id => Some(cached_tabs),
            _ => None,
        }
    }

    /// Fenced: the promotion outdates every fetch in flight, or a poll issued before the switch
    /// heals the tabs back to the space just left.
    fn promote_browsed_tabs(&self, tabs: Vec<TabEntry>) {
        self.begin_layout_fetch();
        let active_key = tabs
            .iter()
            .find(|tab| tab.active)
            .or_else(|| tabs.first())
            .map(|tab| tab.key.clone())
            .unwrap_or_default();
        self.tabs.set(tabs);
        self.active_tab.set(active_key);
    }

    fn select_space_locally(&self, id: &str) {
        let Some(spaces) = self.spaces.try_get_untracked() else {
            return;
        };
        if spaces.iter().any(|space| space.selected && space.id == id) {
            return;
        }
        self.spaces.set(select_by_id(spaces, id));
    }

    /// The re-read keeps `id` selected: `SwitchSpace` acks on enqueue, and nothing else re-lists
    /// spaces to correct a stale answer.
    async fn publish_space_switch(&self, id: &str) {
        self.select_space_locally(id);
        match self.terminal.list_spaces().await {
            Ok(spaces) => {
                if self.signals_alive() {
                    self.spaces.set(select_by_id(spaces, id));
                }
            }
            Err(failure) => self.core.failures().emit(&failure),
        }
    }

    pub async fn focus_pane_from_tap(&self, pane_id: u32, is_plugin: bool) -> bool {
        let tab_key = self.active_tab.get_untracked();
        if !self
            .tell_move(self.terminal.focus_pane(pane_id, is_plugin))
            .await
        {
            return false;
        }
        self.focus_pane_locally(&tab_key, pane_id, is_plugin);
        self.reload_tabs().await;
        true
    }

    /// One focused pane per tab whatever its layer: the sheet shows one badge.
    fn focus_pane_locally(&self, tab_key: &str, pane_id: u32, is_plugin: bool) {
        let Some(current) = self.tabs.try_get_untracked() else {
            return;
        };
        let tabs = current
            .into_iter()
            .map(|tab| {
                if tab.key != tab_key {
                    return tab;
                }
                let panes = tab
                    .panes
                    .into_iter()
                    .map(|pane| PaneEntry {
                        focused: pane.id == pane_id && pane.is_plugin == is_plugin,
                        ..pane
                    })
                    .collect();
                TabEntry { panes, ..tab }
            })
            .collect();
        self.tabs.set(tabs);
    }

    /// Written only from user-driven paths, never from a layout read: right after a reconnect one
    /// reports the daemon-global focus the hint exists to correct.
    fn remember_resume_pane(&self, tab_key: &str, pane_id: u32) {
        self.resume.remember_pane(self.tab_id_for(tab_key), pane_id);
    }

    /// Read-only reaches this: `SwitchSpace` has no read-only gate on the wire. A switch arriving
    /// during another move is dropped whole, since a half-applied switch splits the tab world from
    /// the space list.
    pub async fn select_space(&self, id: String) -> bool {
        let Some(_claim) = self.claim_user_move() else {
            return false;
        };
        self.select_space_unclaimed(id).await
    }

    async fn select_space_unclaimed(&self, id: String) -> bool {
        if !self.tell_move(self.terminal.switch_space(id.clone())).await {
            return false;
        }
        if self.is_herdr_session() {
            self.resume.remember_space(id.clone());
        }
        self.select_space_locally(&id);
        self.load().await;
        self.select_space_locally(&id);
        true
    }

    /// A losing tap answers `false` and opens no dialog; already being on `space_id` needs no claim.
    pub async fn adopt_space(&self, space_id: String) -> bool {
        if self.selected_space_id().as_deref() == Some(space_id.as_str()) {
            return true;
        }
        self.select_space(space_id).await
    }

    async fn adopt_space_unclaimed(&self, space_id: String) -> bool {
        if self.selected_space_id().as_deref() == Some(space_id.as_str()) {
            return true;
        }
        self.select_space_unclaimed(space_id).await
    }

    /// The ack carries no id, so this re-lists spaces and selects a name match locally.
    pub async fn create_space(&self, label: String) {
        let Some(_claim) = self.claim_create() else {
            return;
        };
        if !self.tell(self.terminal.create_space(label.clone())).await {
            return;
        }
        self.settle_create().await;
        match self.terminal.list_spaces().await {
            Ok(spaces) => {
                if self.signals_alive() {
                    self.spaces.set(select_by_label(spaces, &label));
                }
            }
            Err(failure) => self.core.failures().emit(&failure),
        }
    }

    /// A full load: the server focuses what it opened, so the re-seed puts the browse on it.
    pub async fn new_tab(&self, name: String) {
        let Some(_claim) = self.claim_create() else {
            return;
        };
        if self.tell_move(self.terminal.new_tab(name)).await {
            self.settle_create().await;
            self.load().await;
        }
    }

    /// Single-flight over the whole sequence, not just the create: a tap losing only the create
    /// claim would still have moved the session, stranding the winner's pane.
    pub async fn new_pane_in(&self, space_id: Option<String>, tab_key: String, floating: bool) {
        let Some(_claim) = self.claim_user_move() else {
            return;
        };
        if let Some(space_id) = space_id
            && !self.adopt_space_unclaimed(space_id).await
        {
            return;
        }
        let Some(active) = self.active_tab.try_get_untracked() else {
            return;
        };
        if tab_key != active && !self.switch_tab(tab_key).await {
            return;
        }
        self.new_pane(floating).await;
    }

    /// A backend that ignores the flag (herdr) still receives it.
    pub async fn new_pane(&self, floating: bool) {
        let Some(_claim) = self.claim_create() else {
            return;
        };
        if self.tell_move(self.terminal.new_pane(floating)).await {
            self.settle_create().await;
            self.load().await;
        }
    }

    pub async fn rename_space(&self, space_id: String, label: String) {
        if !self.tell(self.terminal.rename_space(space_id, label)).await {
            return;
        }
        match self.terminal.list_spaces().await {
            Ok(spaces) => self.spaces.set(spaces),
            Err(failure) => self.core.failures().emit(&failure),
        }
    }

    pub async fn rename_tab(&self, tab_id: u64, name: String) {
        if self.tell(self.terminal.rename_tab(tab_id, name)).await {
            self.reload_tabs().await;
        }
    }

    pub async fn rename_pane(&self, pane_id: u32, is_plugin: bool, name: String) {
        if self
            .tell(self.terminal.rename_pane(pane_id, is_plugin, name))
            .await
        {
            self.reload_tabs().await;
        }
    }

    /// Off the failure sink: a drag emits one call per line, so a refusing server would toast the whole drag.
    pub async fn scroll_pane_from_wheel(
        &self,
        pane_id: u32,
        is_plugin: bool,
        direction: ScrollStep,
    ) {
        let _ = self
            .terminal
            .scroll_pane(pane_id, is_plugin, direction)
            .await;
    }

    /// [`tell`](Self::tell), not [`tell_move`](Self::tell_move): a fullscreen toggle leaves the
    /// resume view's tab in place. Herdr never reaches the RPC.
    pub async fn toggle_pane_fullscreen_from_pinch(&self, pane_id: u32, is_plugin: bool) -> bool {
        if self.is_herdr_session() {
            return false;
        }
        let Some(_claim) = self.claim_fullscreen_toggle() else {
            return false;
        };
        let landed = self
            .tell(self.terminal.toggle_pane_fullscreen(pane_id, is_plugin))
            .await;
        if landed {
            self.reload_tabs().await;
        }
        landed
    }

    pub async fn reload_tabs(&self) {
        self.refresh_layout(Report::Toast).await;
    }

    pub async fn reload_tabs_quiet(&self) {
        self.refresh_layout(Report::Quiet).await;
    }

    async fn refresh_layout(&self, report: Report) {
        let generation = self.begin_layout_fetch();
        let listed = if FAIL_NEXT_TAB_LIST.swap(false, Ordering::SeqCst) {
            Err(AppFailure::Network(
                "list_tabs failed (test seam)".to_string(),
            ))
        } else {
            self.terminal.list_tabs().await
        };
        match listed {
            Ok(tabs) => {
                if self.publish_if_newest(generation, tabs, Seed::Heal) {
                    self.apply_attach_target().await;
                }
            }
            Err(failure) => match report {
                Report::Toast => self.core.failures().emit(&failure),
                Report::Quiet => log::debug!("list_tabs on attach settle: {failure}"),
                Report::Silent => {}
            },
        }
        if self.agents_relist_due() {
            self.refresh_agents().await;
        } else {
            self.publish_agent_rows();
        }
    }

    /// Always without a subscription; while one is down, flags stale only on the tick a live
    /// stream went down; every [`AGENTS_SAFETY_RELIST`] while live.
    fn agents_relist_due(&self) -> bool {
        let state = lock(&self.agent_watch)
            .as_ref()
            .map(|watch| (watch.end(), watch.is_live()));
        match state {
            None => {
                self.retry_agent_watch();
                true
            }
            Some((end, _)) if end.is_ended() => {
                self.retire_agent_watch(end);
                self.retry_agent_watch();
                true
            }
            Some((_, true)) => {
                self.agent_watch_was_live.store(true, Ordering::SeqCst);
                lock(&self.agents_listed_at).is_none_or(|at| {
                    agents_now().saturating_duration_since(at) >= AGENTS_SAFETY_RELIST
                })
            }
            Some((_, false)) => {
                // The relist flags stale itself if it fails, so a drop with working unary RPCs never flickers it.
                self.agent_watch_was_live.store(false, Ordering::SeqCst);
                true
            }
        }
    }

    /// Never reopens after `UNAUTHENTICATED`; otherwise [`AGENT_WATCH_RETRY`] after it last opened.
    fn retire_agent_watch(&self, end: WatchEnd) {
        let ended = lock(&self.agent_watch).take();
        drop(ended);
        self.agent_watch_was_live.store(false, Ordering::SeqCst);
        let mut reopen = lock(&self.agent_watch_reopen);
        if end == WatchEnd::EndedUnauthenticated {
            *reopen = None;
        } else if let Some(reopen) = reopen.as_mut() {
            reopen.not_before = Some(reopen.opened_at + AGENT_WATCH_RETRY);
        }
    }

    fn retry_agent_watch(&self) {
        let due = lock(&self.agent_watch_reopen).as_ref().and_then(|reopen| {
            reopen
                .not_before
                .is_some_and(|at| agents_now() >= at)
                .then(|| (reopen.controller.upgrade(), reopen.connection.upgrade()))
        });
        if let Some((Some(controller), Some(connection))) = due
            && self.signals_alive()
        {
            controller.open_agent_watch(&connection);
        }
    }

    /// Without [`CAP_AGENTS_STREAM`], or if the open fails, the layout poll keeps reading agents.
    pub fn open_agent_watch(self: &Arc<Self>, connection: &Arc<ConnectionController>) {
        if self.session_id.is_none() || !connection.supports(CAP_AGENTS_STREAM) {
            return;
        }
        let opened_at = agents_now();
        let reopen = |not_before| WatchReopen {
            controller: Arc::downgrade(self),
            connection: Arc::downgrade(connection),
            opened_at,
            not_before,
        };
        let inbox = Arc::new(AgentInbox::default());
        let sender = InboxSender(Arc::clone(&inbox));
        let sink: AgentSink = Box::new(move |event| sender.0.push(event));
        let handle = match self.terminal.watch_agents(sink) {
            Ok(handle) => handle,
            Err(failure) => {
                log::debug!("watch_agents: {failure}");
                *lock(&self.agent_watch_reopen) = Some(reopen(Some(opened_at + AGENT_WATCH_RETRY)));
                return;
            }
        };
        self.agent_watch_was_live.store(false, Ordering::SeqCst);
        *lock(&self.agent_watch_reopen) = Some(reopen(None));
        let previous = lock(&self.agent_watch).replace(handle);
        drop(previous);
        if !self.agent_watch_hooked.swap(true, Ordering::SeqCst) {
            let owner = Arc::downgrade(self);
            self.core.on_dispose(move || {
                if let Some(controller) = owner.upgrade() {
                    lock(&controller.agent_watch_reopen).take();
                    let watch = lock(&controller.agent_watch).take();
                    drop(watch);
                }
            });
        }
        let drain = Arc::downgrade(self);
        spawn_local(async move {
            while let Some(batch) = inbox.next_batch().await {
                let Some(controller) = drain.upgrade() else {
                    return;
                };
                controller
                    .agent_watch_was_live
                    .store(true, Ordering::SeqCst);
                for event in batch {
                    controller.absorb_agent_event(event);
                }
                if inbox.overflowed.swap(false, Ordering::SeqCst) {
                    lock(&controller.agents_listed_at).take();
                }
                controller.agents_confirmed();
            }
        });
    }

    /// Every event clears [`agents_stale`](Self::agents_stale); a held move claim defers only the publish.
    pub fn apply_agent_event(&self, event: AgentEventEntry) {
        self.absorb_agent_event(event);
        self.agents_confirmed();
    }

    /// A change upserts unless the held row's `change_seq` is higher; an in-flight relist is not
    /// cancelled but merges with the panes touched ([`merge_relist`]).
    fn absorb_agent_event(&self, event: AgentEventEntry) {
        let mut rows = lock(&self.agent_rows);
        let mut touched = lock(&self.agents_touched);
        match event {
            AgentEventEntry::Snapshot(agents) => {
                if let Some(touched) = touched.as_mut() {
                    touched.extend(
                        rows.iter()
                            .map(|row| (row.pane_id, Touch::Removed(row.change_seq))),
                    );
                    touched.extend(agents.iter().map(|row| (row.pane_id, Touch::Changed)));
                }
                *rows = agents;
            }
            AgentEventEntry::Changed(agent) => {
                if let Some(touched) = touched.as_mut() {
                    touched.insert(agent.pane_id, Touch::Changed);
                }
                match rows.iter_mut().find(|row| row.pane_id == agent.pane_id) {
                    Some(row) if row.change_seq > agent.change_seq => {}
                    Some(row) => *row = *agent,
                    None => rows.push(*agent),
                }
            }
            AgentEventEntry::Removed(pane_id) => {
                if let Some(touched) = touched.as_mut() {
                    let seq = rows
                        .iter()
                        .find(|row| row.pane_id == pane_id)
                        .map_or(u64::MAX, |row| row.change_seq);
                    touched.insert(pane_id, Touch::Removed(seq));
                }
                rows.retain(|row| row.pane_id != pane_id);
            }
        }
        rows.sort_by_key(|agent| agent_sort_key(agent.status, agent.change_seq));
    }

    fn agents_confirmed(&self) {
        if !self.signals_alive() {
            return;
        }
        self.set_agents_stale(false);
        self.publish_agent_rows();
    }

    fn publish_agent_rows(&self) {
        if self.move_in_flight.load(Ordering::SeqCst) {
            return;
        }
        let rows = lock(&self.agent_rows).clone();
        if self.agents.try_with_untracked(|current| *current != rows) == Some(true) {
            self.agents.set(rows);
        }
    }

    /// Only the newest relist lands. Never published while a move is in flight: a row reordering
    /// under the tapping finger would aim the next tap at another agent.
    async fn refresh_agents(&self) {
        let generation = self.agents_generation.fetch_add(1, Ordering::SeqCst) + 1;
        *lock(&self.agents_touched) = Some(HashMap::new());
        let answer = self.terminal.list_agents().await;
        if self.agents_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let touched = lock(&self.agents_touched).take().unwrap_or_default();
        let listed = match answer {
            Ok(agents) => agents,
            Err(failure) => {
                log::debug!("list_agents: {failure}");
                self.set_agents_stale(true);
                return;
            }
        };
        *lock(&self.agents_listed_at) = Some(agents_now());
        {
            let mut rows = lock(&self.agent_rows);
            *rows = merge_relist(listed, &rows, &touched);
        }
        self.agents_confirmed();
    }

    fn set_agents_stale(&self, stale: bool) {
        if self.agents_stale.try_get_untracked() == Some(!stale) {
            if stale {
                self.agents_stale_raises.fetch_add(1, Ordering::SeqCst);
            }
            self.agents_stale.set(stale);
        }
    }

    pub fn agents_stale_raises(&self) -> u64 {
        self.agents_stale_raises.load(Ordering::SeqCst)
    }

    /// `false` while [`agents_stale`](Self::agents_stale): an unconfirmed list must not claim anything about now.
    pub fn has_blocked_agent(&self) -> bool {
        if self.agents_stale.get() {
            return false;
        }
        self.agents.with(|agents| {
            agents
                .iter()
                .any(|agent| agent.status == AgentStatus::Blocked)
        })
    }

    /// An agent in an unbrowsed space is peeked first: the cross-space commit trusts only a cached browse.
    pub async fn commit_agent(&self, agent: &AgentEntry) -> bool {
        if agent.tab_key.is_empty() {
            return false;
        }
        let selected = self.selected_space_id();
        let Some(space_id) = agent.space_id.clone().or_else(|| selected.clone()) else {
            return self
                .commit_pane(agent.tab_key.clone(), agent.pane_id, false)
                .await;
        };
        if selected.as_deref() == Some(space_id.as_str()) {
            return self
                .commit_pane_in_space(space_id, agent.tab_key.clone(), agent.pane_id, false)
                .await;
        }
        let cached = self.browsed_tabs_for(&space_id);
        let tabs = if let Some(tabs) = cached {
            tabs
        } else {
            let Ok(tabs) = self
                .core
                .run(
                    &self.list_space_tabs,
                    space_id.clone(),
                    RunOptions::default(),
                )
                .await
            else {
                return false;
            };
            if !self.signals_alive() {
                return false;
            }
            tabs
        };
        self.commit_pane_in_space_with(space_id, tabs, agent.tab_key.clone(), agent.pane_id, false)
            .await
    }

    pub fn take_layout_poll_tick(&self) -> bool {
        self.loaded.get_untracked() && !self.layout_poll_in_flight.swap(true, Ordering::SeqCst)
    }

    /// Releases the poll claim through an RAII guard, so an early return cannot wedge the poll shut.
    pub async fn refresh_layout_from_poll(&self) {
        struct ReleasePollClaim<'a>(&'a AtomicBool);
        impl Drop for ReleasePollClaim<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _release = ReleasePollClaim(&self.layout_poll_in_flight);
        self.refresh_layout(Report::Silent).await;
    }

    fn begin_layout_fetch(&self) -> u64 {
        self.layout_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_newest_layout_fetch(&self, generation: u64) -> bool {
        self.layout_generation.load(Ordering::SeqCst) == generation
    }

    fn publish_if_newest(&self, generation: u64, tabs: Vec<TabEntry>, seed: Seed) -> bool {
        if !self.is_newest_layout_fetch(generation) {
            return false;
        }
        self.publish_tabs(tabs, seed);
        self.published_generation
            .store(generation, Ordering::SeqCst);
        true
    }

    /// The one commit point for every layout fetch. Read-compare-then-write: `tabs` is tracked for
    /// the mount's life, so an unconditional `set` rebuilds forever on an idle session.
    fn publish_tabs(&self, tabs: Vec<TabEntry>, seed: Seed) {
        let Some((unchanged, resting)) = self
            .tabs
            .try_with_untracked(|current| (*current == tabs, rest_tab_key(current)))
        else {
            return;
        };
        let mut wrote = false;
        let current = self.active_tab.get_untracked();
        let carries = |key: &str| !key.is_empty() && tabs.iter().any(|tab| tab.key == key);
        let answered = || {
            tabs.iter()
                .find(|tab| tab.active)
                .or_else(|| tabs.first())
                .map(|tab| tab.key.clone())
                .unwrap_or_default()
        };
        let active = match seed {
            Seed::Fresh => answered(),
            Seed::Heal if carries(&current) => current.clone(),
            Seed::Heal => answered(),
        };
        if current != active {
            self.active_tab.set(active.clone());
            wrote = true;
        }
        let moved = rest_tab_key(&tabs);
        if !unchanged {
            self.tabs.set(tabs);
            wrote = true;
        }
        // Docked, the rail follows another client's focus only while resting: a browse is the user's.
        if self.is_docked()
            && moved != resting
            && self.expanded_tab.get_untracked() == resting
            && self.expansion_on_selected_space()
        {
            self.expanded_tab.set(moved);
        }
        if wrote {
            self.layout_writes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Tab keys are unique only within one layout, so a same-keyed tab under a browsed space is not the resting one.
    fn expansion_on_selected_space(&self) -> bool {
        !self.is_herdr_session() || self.expanded_space.get_untracked() == self.selected_space_id()
    }

    /// Public: the tree sheet, a root overlay page, can outlive the screen. One owner answers for every signal.
    pub fn signals_alive(&self) -> bool {
        self.tabs.try_with_untracked(|_| ()).is_some()
    }

    /// Releases on drop, so a refused create re-arms the affordance.
    fn claim_create(&self) -> Option<FlightClaim<'_>> {
        (!self.create_in_flight.swap(true, Ordering::SeqCst))
            .then(|| FlightClaim(&self.create_in_flight))
    }

    fn claim_user_move(&self) -> Option<FlightClaim<'_>> {
        self.claim_move()
    }

    /// One slot shared by the sheet's commit and every space switch: a switch landing mid-commit
    /// would strand that commit's steps in a space the user never pointed at.
    fn claim_move(&self) -> Option<FlightClaim<'_>> {
        (!self.move_in_flight.swap(true, Ordering::SeqCst))
            .then(|| FlightClaim(&self.move_in_flight))
    }

    fn claim_fullscreen_toggle(&self) -> Option<FlightClaim<'_>> {
        (!self
            .fullscreen_toggle_in_flight
            .swap(true, Ordering::SeqCst))
        .then(|| FlightClaim(&self.fullscreen_toggle_in_flight))
    }

    async fn settle_create(&self) {
        clean_signals::time::sleep(create_settle()).await;
    }

    pub fn layout_writes(&self) -> u64 {
        self.layout_writes.load(Ordering::Relaxed)
    }

    /// Only herdr has spaces; zellij's `GetSpaces` answers an empty list.
    pub fn has_spaces(&self) -> bool {
        self.is_herdr_session()
    }

    /// Off the session id: the backend is a fact about the session, not about an open stream.
    pub fn is_herdr_session(&self) -> bool {
        self.session_id.as_deref().is_some_and(is_herdr)
    }

    fn tab_id_for(&self, key: &str) -> Option<u64> {
        self.tabs
            .try_get_untracked()?
            .into_iter()
            .find(|tab| tab.key == key)
            .map(|tab| tab.tab_id)
    }

    fn selected_space_id(&self) -> Option<String> {
        self.spaces
            .try_get_untracked()?
            .into_iter()
            .find(|space| space.selected)
            .map(|space| space.id)
    }

    async fn tell(&self, work: impl Future<Output = Result<(), AppFailure>>) -> bool {
        match work.await {
            Ok(()) => true,
            Err(failure) => {
                self.core.failures().emit(&failure);
                false
            }
        }
    }

    /// The one place the supersede rule acts; holds `focus_verbs_in_flight` across `work`.
    async fn tell_move(&self, work: impl Future<Output = Result<(), AppFailure>>) -> bool {
        self.focus_verbs_in_flight.fetch_add(1, Ordering::SeqCst);
        let _in_flight = VerbGuard(&self.focus_verbs_in_flight);
        let landed = self.tell(work).await;
        if landed {
            self.supersede_attach_target();
        }
        landed
    }

    /// Writes through the sessions repository so the list reflects it.
    pub async fn rename(&self, name: String) -> Option<String> {
        let id = self.session_id.clone()?;
        let session = self
            .core
            .run(
                &self.rename,
                RenameRequest { id, name },
                RunOptions::default(),
            )
            .await
            .ok()?;
        self.session_name.set(Some(session.name.clone()));
        Some(session.name)
    }

    pub async fn detach(&self) -> bool {
        let Some(id) = self.session_id.clone() else {
            return false;
        };
        self.core
            .run(&self.detach, id, RunOptions::default())
            .await
            .is_ok()
    }

    pub fn toggle_fullscreen(&self) -> bool {
        let next = !self.fullscreen.get_untracked();
        self.fullscreen.set(next);
        next
    }

    pub fn active_tab_entry(&self) -> Option<TabEntry> {
        let key = self.active_tab.get_untracked();
        self.tabs
            .get_untracked()
            .into_iter()
            .find(|tab| tab.key == key)
    }

    pub fn crumb_labels(&self) -> Vec<String> {
        let space = self
            .spaces
            .get()
            .into_iter()
            .find(|space| space.selected)
            .map(|space| space.name);
        let key = self.active_tab.get();
        let tab = self
            .tabs
            .get()
            .into_iter()
            .find(|tab| tab.key == key)
            .map(|tab| tab.label);
        [self.session_name.get(), space, tab]
            .into_iter()
            .flatten()
            .collect()
    }

    /// One rebuild per replay, zero per frame.
    fn replay_reveal(&self) {
        self.reveal_epoch
            .set(self.next_epoch.fetch_add(1, Ordering::Relaxed));
    }

    /// Idempotent, and never attaches to a real session: the seam the live tests drive the grid through.
    pub fn enter_live(&self) {
        self.live.set(true);
    }

    /// Claims the pane before flipping the flag, so the first build that observes it picks the real feed.
    pub fn attach_live(&self) {
        self.live_attached.store(true, Ordering::Relaxed);
        self.live.set(true);
    }

    pub fn attached_session(&self) -> Option<String> {
        if self.live_attached.load(Ordering::Relaxed) {
            self.session_id.clone()
        } else {
            None
        }
    }

    /// Read-compare-then-write: [`link`](Self::link) is tracked in `build`. Answers `true` on the
    /// first wake with a real connection id, the caller's cue for a catch-up reload.
    pub fn mirror_link(&self, status: LinkStatus, connection_id: Option<String>) -> bool {
        if self.link.get_untracked() != Some(status) {
            self.link.set(Some(status));
        }
        let connection_id = connection_id.unwrap_or_default();
        let mut bound = self
            .bound_connection_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *bound != connection_id {
            connection_id.clone_into(&mut bound);
            self.terminal
                .bind(self.session_id.clone(), Some(connection_id.clone()));
        }
        let settled = (!connection_id.is_empty() || status == LinkStatus::Connected)
            && self
                .attach_settled
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
        if settled {
            let current = self.layout_generation.load(Ordering::SeqCst);
            self.settle_generation.store(current, Ordering::SeqCst);
        }
        settled
    }

    /// Read-compare-then-write, since all three are tracked in `build`. Each tick spawns detached
    /// from the owner, so its first read is `try_get_untracked`.
    pub async fn refresh_appearance(&self) {
        let Ok(prefs) = self.preferences.load_preferences().await else {
            return;
        };
        let Some(current_cursor) = self.cursor_style.try_get_untracked() else {
            return;
        };
        let mut wrote = false;
        let cursor = map_cursor_style(prefs.appearance.cursor_style);
        if current_cursor != cursor {
            self.cursor_style.set(cursor);
            wrote = true;
        }
        let font_size = prefs.appearance.font_size_px as f32;
        if self.font_size.get_untracked() != font_size {
            self.font_size.set(font_size);
            wrote = true;
        }
        let font_family = fonts::resolve_terminal_family(&prefs.appearance.font_family);
        if self.font_family.get_untracked() != font_family {
            self.font_family.set(font_family);
            wrote = true;
        }
        if wrote {
            self.appearance_writes.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn appearance_writes(&self) -> u64 {
        self.appearance_writes.load(Ordering::Relaxed)
    }
}

/// Released on drop, so it covers the whole verb however it returns.
struct FlightClaim<'a>(&'a AtomicBool);

impl Drop for FlightClaim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Releases one `focus_verbs_in_flight` count on every exit path.
struct VerbGuard<'a>(&'a AtomicUsize);

impl Drop for VerbGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

const HERDR_BACKEND: &str = "herdr:";

/// Recovery only: unlike an in-place focus verb, a fresh attach's resume hint is best-effort,
/// so herdr risks landing on the daemon's global focus.
pub type Reattach = Rc<dyn Fn()>;

/// Lock `mutex`, recovering a poisoned one: a panic elsewhere must not wedge the agents state.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The relist is authoritative for every pane the stream left alone. A pane the stream changed
/// keeps the higher `change_seq` (the relist's on a tie); a removed pane stays gone unless the
/// relist's `change_seq` beats the removed row's.
fn merge_relist(
    listed: Vec<AgentEntry>,
    held: &[AgentEntry],
    touched: &HashMap<u32, Touch>,
) -> Vec<AgentEntry> {
    let held_row = |pane_id: u32| held.iter().find(|row| row.pane_id == pane_id);
    let mut rows: Vec<AgentEntry> = listed
        .into_iter()
        .filter(|row| match touched.get(&row.pane_id) {
            Some(Touch::Removed(seq)) => row.change_seq > *seq,
            _ => true,
        })
        .map(
            |row| match (touched.get(&row.pane_id), held_row(row.pane_id)) {
                (Some(Touch::Changed), Some(kept)) if kept.change_seq > row.change_seq => {
                    kept.clone()
                }
                _ => row,
            },
        )
        .collect();
    for kept in held {
        let changed = touched.get(&kept.pane_id) == Some(&Touch::Changed);
        if changed && !rows.iter().any(|row| row.pane_id == kept.pane_id) {
            rows.push(kept.clone());
        }
    }
    rows.sort_by_key(|agent| agent_sort_key(agent.status, agent.change_seq));
    rows
}

fn rest_tab_key(tabs: &[TabEntry]) -> Option<String> {
    tabs.iter()
        .find(|tab| tab.active)
        .map(|tab| tab.key.clone())
}

fn resolve_attach_target<'a>(
    tabs: &'a [TabEntry],
    active: &str,
    target: &AttachTarget,
) -> Option<(&'a TabEntry, Option<u32>)> {
    // `TabEntry::index` is the 1-based position the pill prints.
    let tab = match target.tab_position {
        Some(position) => tabs
            .iter()
            .find(|tab| tab.index == position.saturating_add(1)),
        None => tabs.iter().find(|tab| tab.key == active),
    };
    let Some(tab) = tab else {
        log::debug!(
            "attach target: no tab at {:?}, dropped",
            target.tab_position
        );
        return None;
    };
    if let Some(pane_id) = target.pane_id
        && !tab
            .panes
            .iter()
            .any(|pane| pane.id == pane_id && !pane.is_plugin)
    {
        log::debug!("attach target: no pane {pane_id} in tab, dropped");
        return None;
    }
    Some((tab, target.pane_id))
}

/// Read off the id's `<backend>:<name>` convention (`SessionInfo.id`).
fn is_herdr(session: &str) -> bool {
    session.starts_with(HERDR_BACKEND)
}

/// A landed switch's own ask outranks a list answered before the relay applied it. A non-match
/// changes nothing, so a list that dropped `id` keeps its selection.
fn select_by_id(spaces: Vec<Space>, id: &str) -> Vec<Space> {
    if !spaces.iter().any(|space| space.id == id) {
        return spaces;
    }
    spaces
        .into_iter()
        .map(|space| Space {
            selected: space.id == id,
            ..space
        })
        .collect()
}

fn select_by_label(spaces: Vec<Space>, label: &str) -> Vec<Space> {
    if !spaces.iter().any(|space| space.name == label) {
        return spaces;
    }
    spaces
        .into_iter()
        .map(|space| Space {
            selected: space.name == label,
            ..space
        })
        .collect()
}

/// Held concretely so the half not on the [`TerminalFeed`] port stays reachable.
pub enum LiveFeed {
    Demo(Rc<RefCell<DemoFeed>>),
    Grpc(Rc<RefCell<GrpcTerminalFeed>>),
}

impl LiveFeed {
    /// A second handle onto the same cell, so a re-attach is visible through every trait object.
    fn shared(&self) -> term::SharedFeed {
        match self {
            LiveFeed::Demo(feed) => feed.clone(),
            LiveFeed::Grpc(feed) => feed.clone(),
        }
    }

    pub fn demo(&self) -> Option<&Rc<RefCell<DemoFeed>>> {
        match self {
            LiveFeed::Demo(feed) => Some(feed),
            LiveFeed::Grpc(_) => None,
        }
    }

    pub fn status(&self) -> Option<LinkStatus> {
        match self {
            LiveFeed::Demo(_) => None,
            LiveFeed::Grpc(feed) => Some(feed.borrow().status()),
        }
    }
}

/// Not a controller field: its `Rc` handles would make the controller `!Send`. Dropping it is
/// the detach: [`GrpcTerminalFeed`]'s `Drop` shuts the request stream.
pub struct LiveSession {
    pub emulator: term::SharedEmulator,
    pub feed: LiveFeed,
    pub input: term::InputSeam,
    pub selection: term::SelectionSeam,
    /// A tracked read in `Component::build` is what wakes the live pane's repaint.
    pub generation: RwSignal<u64>,
    pub reattach: Option<Reattach>,
}

struct AttachSpec {
    connection: Arc<ConnectionController>,
    session: String,
    /// Fires on the background runtime, where writing a signal is forbidden: it only raises a flag.
    auth_expired: Arc<AtomicBool>,
    resume: Arc<ResumeSlot>,
    /// So an auth expiry is fenced to the client the newest feed attached with, not whatever is live now.
    client: Arc<Mutex<Option<MuxrClient>>>,
}

impl LiveSession {
    /// Must run under the screen's component owner (`Component::build`, never a press handler), so
    /// the pump's and poll's `on_cleanup` fire on unmount.
    pub fn enter(
        controller: &Arc<TerminalController>,
        connection: &Arc<ConnectionController>,
    ) -> Self {
        let emulator: term::SharedEmulator =
            Rc::new(RefCell::new(term::TerminalEmulator::with_defaults()));
        let attach = controller.attached_session().and_then(|session| {
            let spec = AttachSpec {
                connection: Arc::clone(connection),
                session,
                auth_expired: Arc::new(AtomicBool::new(false)),
                resume: controller.resume_slot(),
                client: Arc::new(Mutex::new(None)),
            };
            let (cols, rows) = emulator.borrow().size();
            let feed = spec.attach(cols, rows)?;
            Some((Rc::new(RefCell::new(feed)), spec))
        });

        let (feed, spec) = match attach {
            Some((feed, spec)) => (LiveFeed::Grpc(feed), Some(spec)),
            None => (LiveFeed::Demo(Rc::new(RefCell::new(DemoFeed::new()))), None),
        };
        let feed_dyn: term::SharedFeed = feed.shared();
        let generation = RwSignal::new(0u64);
        let mut input = term::InputSeam::new(feed_dyn.clone());
        // Without the echo drain a typed keystroke waits on the next feed wake.
        input.wire_pump(Rc::clone(&emulator), generation);
        let selection = term::SelectionSeam::new(Rc::clone(&emulator));

        let reattach = match (&feed, spec) {
            (LiveFeed::Grpc(grpc), Some(spec)) => {
                // Rides the repaint pump: already awake and on the UI thread.
                let watch = Rc::clone(grpc);
                let mirror = Arc::clone(controller);
                let expiry = Arc::clone(&spec.connection);
                let flag = Arc::clone(&spec.auth_expired);
                let feed_client = Arc::clone(&spec.client);
                term::spawn_feed_pump_watched(
                    feed_dyn,
                    Rc::clone(&emulator),
                    generation,
                    move || {
                        let (status, connection_id) = {
                            let feed = watch.borrow();
                            (feed.status(), feed.connection_id())
                        };
                        if mirror.mirror_link(status, connection_id) {
                            // Settled: re-read the layout, since the mount's own load had no connection id.
                            let settled = Arc::clone(&mirror);
                            spawn_local(async move {
                                settled.reload_tabs_quiet().await;
                                settled.apply_attach_target().await;
                            });
                        }
                        if flag.swap(false, Ordering::AcqRel) {
                            match lock(&feed_client).clone() {
                                Some(client) => expiry.on_session_expired_for(&client),
                                None => expiry.on_session_expired(),
                            }
                        }
                    },
                );
                Some(spec.hook(grpc, &emulator))
            }
            _ => {
                term::spawn_feed_pump(feed_dyn, Rc::clone(&emulator), generation);
                None
            }
        };

        let now = Arc::clone(controller);
        spawn_local(async move { now.refresh_appearance().await });
        let poll = Arc::clone(controller);
        use_interval(APPEARANCE_POLL_INTERVAL, move || {
            let poll = Arc::clone(&poll);
            spawn_local(async move { poll.refresh_appearance().await });
        });

        // A fixture replay fronts no server whose tab world could move.
        if matches!(feed, LiveFeed::Grpc(_)) {
            let layout = Arc::clone(controller);
            use_interval(LAYOUT_POLL_INTERVAL, move || {
                if !layout.take_layout_poll_tick() {
                    return;
                }
                let layout = Arc::clone(&layout);
                spawn_local(async move { layout.refresh_layout_from_poll().await });
            });
        }
        controller.open_agent_watch(connection);

        Self {
            emulator,
            feed,
            input,
            selection,
            generation,
            reattach,
        }
    }

    pub fn status(&self) -> Option<LinkStatus> {
        self.feed.status()
    }

    pub fn reattach(&self) {
        if let Some(reattach) = &self.reattach {
            reattach();
        }
    }

    /// A parked schedule re-arms through `reset_backoff`; a session ended for good (an expired
    /// credential) has no pump left, so only a fresh stream works.
    pub fn retry(&self) {
        let LiveFeed::Grpc(feed) = &self.feed else {
            return;
        };
        let parked = {
            let feed = feed.borrow();
            feed.reset_backoff();
            feed.is_open()
        };
        if !parked {
            self.reattach();
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.feed.shared().borrow_mut().resize(cols, rows);
    }

    pub fn feed_handle(&self) -> term::SharedFeed {
        self.feed.shared()
    }
}

impl AttachSpec {
    /// `None` when nothing is connected: a torn-down connection degrades rather than panics.
    fn attach(&self, cols: u16, rows: u16) -> Option<GrpcTerminalFeed> {
        let client = self.connection.client()?;
        *lock(&self.client) = Some(client.clone());
        let flag = Arc::clone(&self.auth_expired);
        Some(GrpcTerminalFeed::attach(
            client,
            self.session.clone(),
            cols,
            rows,
            Box::new(move || flag.store(true, Ordering::Release)),
            Arc::clone(&self.resume),
        ))
    }

    /// Replaces the feed in place, so the pump and seams keep working and the old stream drops.
    fn hook(
        self,
        feed: &Rc<RefCell<GrpcTerminalFeed>>,
        emulator: &term::SharedEmulator,
    ) -> Reattach {
        let feed = Rc::clone(feed);
        let emulator = Rc::clone(emulator);
        Rc::new(move || {
            let (cols, rows) = emulator.borrow().size();
            let Some(fresh) = self.attach(cols, rows) else {
                return;
            };
            *feed.borrow_mut() = fresh;
        })
    }
}

trait ActiveTab {
    fn update_active(&self, key: &str);
}

impl ActiveTab for RwSignal<Vec<TabEntry>> {
    fn update_active(&self, key: &str) {
        let Some(current) = self.try_get_untracked() else {
            return;
        };
        let tabs = current
            .into_iter()
            .map(|tab| TabEntry {
                active: tab.key == key,
                ..tab
            })
            .collect();
        self.set(tabs);
    }
}

impl AsRef<ControllerCore<AppFailure>> for TerminalController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The one place terminal's `presentation` names a sibling feature's `data`.
    use std::collections::VecDeque;

    use crate::features::sessions::data::repositories::MockSessionsRepository;
    use crate::features::sessions::data::repositories::mock_sessions_repository::SEEDED_DEV_ID;
    use crate::features::settings::data::repositories::MockPreferencesRepository;
    use crate::features::settings::domain::entities::AppearancePrefs;
    use crate::features::terminal::data::feeds::grpc_feed::ResumeTarget;
    use crate::features::terminal::data::repositories::MockTerminalRepository;

    fn controller(session_id: Option<&str>) -> TerminalController {
        TerminalController::new(
            Arc::new(MockTerminalRepository::seeded()),
            Arc::new(MockSessionsRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            session_id.map(str::to_string),
        )
    }

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn a_load_lands_on_the_mocks_active_tab() {
        let c = controller(Some(SEEDED_DEV_ID));
        assert!(!c.loaded.get_untracked());

        block_on(c.load());

        assert!(c.loaded.get_untracked());
        assert!(
            c.spaces.get_untracked().is_empty(),
            "a zellij session has no spaces, and its load never asks for any"
        );
        assert_eq!(c.tabs.get_untracked().len(), 4);
        assert_eq!(c.active_tab.get_untracked(), "devbox");
        assert_eq!(
            c.active_tab_entry().map(|t| t.pane_title),
            Some("ed@devbox: ~".to_string())
        );
        assert_eq!(
            c.lines.get_untracked().first().map(|l| l.text.clone()),
            Some("ed@devbox:~$ systemctl status muxrd".to_string())
        );
        assert_eq!(c.session_name.get_untracked().as_deref(), Some("dev"));
    }

    #[test]
    fn only_a_herdr_session_reads_the_space_list() {
        let herdr = controller(Some("herdr:work"));
        block_on(herdr.load());
        assert!(herdr.has_spaces());
        assert_eq!(herdr.spaces.get_untracked().len(), 3);

        let zellij = controller(Some(SEEDED_DEV_ID));
        block_on(zellij.load());
        assert!(!zellij.has_spaces());
        assert!(zellij.spaces.get_untracked().is_empty());

        assert!(!controller(None).has_spaces());
    }

    #[test]
    fn switching_a_tab_swaps_the_lines_and_replays_the_reveal() {
        let c = controller(None);
        block_on(c.load());
        let before = c.reveal_epoch.get_untracked();

        block_on(c.switch_tab("btop".to_string()));

        assert_eq!(c.active_tab.get_untracked(), "btop");
        assert_eq!(
            c.lines.get_untracked().first().map(|l| l.text.clone()),
            Some("ed@workbench:~$ btop".to_string())
        );
        assert!(
            c.reveal_epoch.get_untracked() > before,
            "the replay token advances on every switch"
        );
        let active: Vec<String> = c
            .tabs
            .get_untracked()
            .into_iter()
            .filter(|t| t.active)
            .map(|t| t.key)
            .collect();
        assert_eq!(active, ["btop"]);
    }

    #[test]
    fn switching_to_an_unknown_tab_changes_nothing() {
        let c = controller(None);
        block_on(c.load());
        let epoch = c.reveal_epoch.get_untracked();

        block_on(c.switch_tab("nope".to_string()));

        assert_eq!(c.active_tab.get_untracked(), "devbox");
        assert_eq!(
            c.reveal_epoch.get_untracked(),
            epoch,
            "no replay on failure"
        );
    }

    #[test]
    fn selecting_a_space_moves_the_selection() {
        let c = controller(Some("herdr:work"));
        block_on(c.load());

        block_on(c.select_space("quill".to_string()));

        let selected: Vec<String> = c
            .spaces
            .get_untracked()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.name)
            .collect();
        assert_eq!(selected, ["quill"]);
    }

    #[test]
    fn a_space_switch_the_backend_refuses_leaves_the_selection_where_it_was() {
        let c = controller(Some("herdr:work"));
        block_on(c.load());

        block_on(c.select_space("no-such-space".to_string()));

        let selected: Vec<String> = c
            .spaces
            .get_untracked()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.name)
            .collect();
        assert_eq!(
            selected,
            ["forgekit"],
            "the sheet must not show a space the session is not on"
        );
    }

    #[test]
    fn a_rename_round_trips_through_the_sessions_repository() {
        let sessions = Arc::new(MockSessionsRepository::seeded());
        let c = TerminalController::new(
            Arc::new(MockTerminalRepository::seeded()),
            Arc::clone(&sessions) as Arc<dyn SessionsRepository + Send + Sync>,
            Arc::new(MockPreferencesRepository::seeded()),
            Some(SEEDED_DEV_ID.to_string()),
        );
        block_on(c.load());

        assert_eq!(
            block_on(c.rename("dev-2".to_string())).as_deref(),
            Some("dev-2")
        );

        assert_eq!(c.session_name.get_untracked().as_deref(), Some("dev-2"));
        let names: Vec<String> = sessions.snapshot().into_iter().map(|s| s.name).collect();
        assert!(
            names.contains(&"dev-2".to_string()),
            "the sessions list sees it too: {names:?}"
        );
    }

    #[test]
    fn a_rename_without_an_attached_session_is_a_no_op() {
        let c = controller(None);
        block_on(c.load());
        assert_eq!(block_on(c.rename("whatever".to_string())), None);
        assert_eq!(c.session_name.get_untracked(), None);
    }

    #[test]
    fn detach_needs_an_attached_session() {
        let attached = controller(Some(SEEDED_DEV_ID));
        assert!(block_on(attached.detach()));

        let bare = controller(None);
        assert!(!block_on(bare.detach()), "nothing to detach from");
    }

    #[test]
    fn fullscreen_toggles_both_ways() {
        let c = controller(None);
        assert!(!c.fullscreen.get_untracked());
        assert!(c.toggle_fullscreen());
        assert!(c.fullscreen.get_untracked());
        assert!(!c.toggle_fullscreen());
        assert!(!c.fullscreen.get_untracked());
    }

    #[test]
    fn entering_live_flips_the_one_flag() {
        let c = controller(None);
        assert!(!c.live.get_untracked());
        c.enter_live();
        assert!(c.live.get_untracked());
    }

    #[test]
    fn only_attaching_claims_the_live_pane_for_a_real_session() {
        let demo = controller(Some(SEEDED_DEV_ID));
        demo.enter_live();
        assert!(demo.live.get_untracked());
        assert_eq!(demo.attached_session(), None);

        let attached = controller(Some(SEEDED_DEV_ID));
        attached.attach_live();
        assert!(attached.live.get_untracked());
        assert_eq!(attached.attached_session().as_deref(), Some(SEEDED_DEV_ID));
    }

    #[test]
    fn a_bare_terminal_route_has_no_session_to_attach_to() {
        let c = controller(None);
        c.attach_live();
        assert_eq!(c.attached_session(), None);
    }

    #[test]
    fn the_backend_discriminator_reads_the_sessions_own_id_prefix() {
        assert!(is_herdr("herdr:work"));
        assert!(!is_herdr(SEEDED_DEV_ID));
        assert!(!is_herdr("dev"));

        let c = controller(Some("herdr:work"));
        assert!(
            c.is_herdr_session(),
            "which backend a session runs on is a fact about the session, not \
             about whether a stream is open onto it"
        );
        assert!(c.has_spaces());
        assert!(!controller(Some(SEEDED_DEV_ID)).is_herdr_session());
        assert!(!controller(None).is_herdr_session());
    }

    #[test]
    fn a_tab_switch_reports_whether_it_landed() {
        let c = controller(Some(SEEDED_DEV_ID));
        block_on(c.load());
        assert!(block_on(c.switch_tab("btop".to_string())));
        assert_eq!(c.active_tab.get_untracked(), "btop");
    }

    #[test]
    fn a_switch_to_a_tab_the_sheet_is_no_longer_showing_changes_nothing() {
        let c = controller(Some("herdr:work"));
        c.attach_live();
        block_on(c.load());
        let epoch = c.reveal_epoch.get_untracked();

        assert!(!block_on(c.switch_tab("gone".to_string())));

        assert_eq!(c.active_tab.get_untracked(), "devbox");
        assert_eq!(c.reveal_epoch.get_untracked(), epoch);
    }

    #[test]
    fn the_link_starts_unreported_and_mirrors_read_compare_then_write() {
        let c = controller(Some(SEEDED_DEV_ID));
        assert_eq!(
            c.link.get_untracked(),
            None,
            "a screen with no live session reports no link"
        );

        c.mirror_link(LinkStatus::Reconnecting, None);
        assert_eq!(c.link.get_untracked(), Some(LinkStatus::Reconnecting));

        c.mirror_link(LinkStatus::Connected, Some("conn-1".to_string()));
        assert_eq!(c.link.get_untracked(), Some(LinkStatus::Connected));
    }

    #[test]
    fn a_minted_connection_id_re_binds_the_repository_and_an_unchanged_one_does_not() {
        let repository = Arc::new(RecordingRepository::default());
        let c = TerminalController::new(
            Arc::clone(&repository) as Arc<dyn TerminalRepository + Send + Sync>,
            Arc::new(MockSessionsRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            Some(SEEDED_DEV_ID.to_string()),
        );
        assert_eq!(
            repository.binds(),
            1,
            "construction binds the session, before any stream exists"
        );

        c.mirror_link(LinkStatus::Connected, Some("conn-1".to_string()));
        assert_eq!(repository.binds(), 2);
        c.mirror_link(LinkStatus::Connected, Some("conn-1".to_string()));
        c.mirror_link(LinkStatus::Reconnecting, Some("conn-1".to_string()));
        assert_eq!(repository.binds(), 2, "an unchanged id re-binds nothing");

        c.mirror_link(LinkStatus::Connected, Some("conn-2".to_string()));
        assert_eq!(repository.binds(), 3);
        assert_eq!(repository.last_binding(), Some("conn-2".to_string()));
    }

    /// Records every call, so a flow's RPC sequence is assertable.
    #[derive(Default)]
    struct RecordingRepository {
        calls: Mutex<Vec<String>>,
        last: Mutex<Option<String>>,
        refuse: Mutex<Option<String>>,
        delays: Mutex<VecDeque<Duration>>,
        /// Off by default: a stamped answer differs on every call and defeats the unchanged-answer tests.
        lists: AtomicU64,
        stamp: AtomicBool,
        created: AtomicBool,
        /// Re-applied to later `list_tabs`: a server hands a client back the focus it asked for.
        focused: Mutex<Option<(u32, bool)>>,
        selected: Mutex<Option<String>>,
        active: Mutex<Option<String>>,
        agents: Mutex<Vec<AgentEntry>>,
        agents_delay: Mutex<Option<Duration>>,
    }

    const SEEDED_SPACE: &str = "ws-1";

    impl RecordingRepository {
        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn actions(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|call| {
                    !call.starts_with("bind")
                        && !call.starts_with("list_")
                        && !call.starts_with("tab_lines")
                })
                .collect()
        }

        fn binds(&self) -> usize {
            self.calls()
                .iter()
                .filter(|call| call.starts_with("bind"))
                .count()
        }

        fn last_binding(&self) -> Option<String> {
            self.last
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn refuse(&self, verb: &str) {
            *self.refuse.lock().unwrap_or_else(PoisonError::into_inner) = Some(verb.to_string());
        }

        fn delay_calls(&self, delays: impl IntoIterator<Item = Duration>) {
            self.delays
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(delays);
        }

        fn stamp_lists(&self) {
            self.stamp.store(true, Ordering::Relaxed);
        }

        fn report_focus(&self, focus: (u32, bool)) {
            *self.focused.lock().unwrap_or_else(PoisonError::into_inner) = Some(focus);
        }

        fn report_active(&self, key: &str) {
            *self.active.lock().unwrap_or_else(PoisonError::into_inner) = Some(key.to_string());
        }

        async fn park(&self) {
            let delay = self
                .delays
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            if let Some(delay) = delay {
                clean_signals::time::sleep(delay).await;
            }
        }

        fn record(&self, call: String) -> Result<(), AppFailure> {
            let refused = self
                .refuse
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_deref()
                .is_some_and(|verb| call.starts_with(verb));
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(call.clone());
            if refused {
                return Err(AppFailure::Internal(format!("{call} refused")));
            }
            Ok(())
        }
    }

    /// Two panes per tab: `tab_id * 10` (focused) and one past it.
    fn recorded_tab(index: u32, key: &str, active: bool) -> TabEntry {
        let base = index * 10;
        let pane = |id: u32, focused: bool| PaneEntry {
            agent: None,
            id,
            title: format!("{key}-{id}"),
            cwd: format!("/srv/{key}"),
            focused,
            floating: false,
            exited: false,
            is_plugin: false,
            is_fullscreen: false,
            x: 0,
            y: 0,
            rows: 24,
            cols: 80,
        };
        TabEntry {
            index,
            key: key.to_string(),
            tab_id: u64::from(index),
            label: key.to_string(),
            pane_title: key.to_string(),
            pane_name: key.to_string(),
            pane_path: format!("/srv/{key}"),
            panes: vec![pane(base, true), pane(base + 1, false)],
            active,
            has_bell: false,
            floating_panes_visible: false,
            fullscreen_active: false,
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
    #[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
    impl TerminalRepository for RecordingRepository {
        fn bind(&self, _session: Option<String>, connection_id: Option<String>) {
            let _ = self.record(format!(
                "bind({})",
                connection_id.clone().unwrap_or_default()
            ));
            *self.last.lock().unwrap_or_else(PoisonError::into_inner) = connection_id;
        }

        async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
            self.record("list_spaces".to_string())?;
            self.park().await;
            let selected = self
                .selected
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or_else(|| SEEDED_SPACE.to_string());
            Ok(vec![
                Space {
                    id: "ws-1".to_string(),
                    name: "one".to_string(),
                    selected: selected == "ws-1",
                },
                Space {
                    id: "ws-2".to_string(),
                    name: "two".to_string(),
                    selected: selected == "ws-2",
                },
            ])
        }

        async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
            self.record("list_tabs".to_string())?;
            // Taken before the park, so the answer carries the round trip that started first.
            let call = self.lists.fetch_add(1, Ordering::SeqCst) + 1;
            self.park().await;
            let mut tabs = vec![
                recorded_tab(1, "alpha", true),
                recorded_tab(2, "beta", false),
            ];
            if self.created.load(Ordering::SeqCst) {
                tabs.push(recorded_tab(3, "gamma", false));
            }
            let focused = *self.focused.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((pane_id, is_plugin)) = focused {
                let addressed =
                    |pane: &PaneEntry| pane.id == pane_id && pane.is_plugin == is_plugin;
                if let Some(tab) = tabs.iter_mut().find(|tab| tab.panes.iter().any(&addressed)) {
                    for pane in &mut tab.panes {
                        pane.focused = addressed(pane);
                    }
                }
            }
            if self.stamp.load(Ordering::Relaxed) {
                for tab in &mut tabs {
                    tab.label = format!("{}#{call}", tab.key);
                }
            }
            if let Some(key) = self
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_deref()
            {
                for tab in &mut tabs {
                    tab.active = tab.key == key;
                }
            }
            Ok(tabs)
        }

        async fn list_agents(&self) -> Result<Vec<AgentEntry>, AppFailure> {
            let delay = *self
                .agents_delay
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(delay) = delay {
                clean_signals::time::sleep(delay).await;
            }
            Ok(self
                .agents
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone())
        }

        async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure> {
            self.record(format!("tab_lines({key})"))?;
            self.park().await;
            Ok(Vec::new())
        }

        async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
            self.record(format!("switch_space({space_id})"))?;
            *self.selected.lock().unwrap_or_else(PoisonError::into_inner) = Some(space_id);
            Ok(())
        }

        async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
            self.record(format!("go_to_tab({tab_id})"))
        }

        async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
            self.record(format!("detach_session({id})"))
        }

        async fn focus_pane(&self, pane_id: u32, is_plugin: bool) -> Result<(), AppFailure> {
            self.record(format!("focus_pane({pane_id},{is_plugin})"))?;
            *self.focused.lock().unwrap_or_else(PoisonError::into_inner) =
                Some((pane_id, is_plugin));
            Ok(())
        }

        async fn scroll_pane(
            &self,
            pane_id: u32,
            is_plugin: bool,
            direction: ScrollStep,
        ) -> Result<(), AppFailure> {
            self.record(format!("scroll_pane({pane_id},{is_plugin},{direction:?})"))
        }

        async fn create_space(&self, label: String) -> Result<(), AppFailure> {
            self.record(format!("create_space({label})"))?;
            self.park().await;
            Ok(())
        }

        async fn rename_space(&self, space_id: String, label: String) -> Result<(), AppFailure> {
            self.record(format!("rename_space({space_id},{label})"))
        }

        async fn new_tab(&self, name: String) -> Result<(), AppFailure> {
            self.record(format!("new_tab({name})"))?;
            self.park().await;
            self.created.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn rename_tab(&self, tab_id: u64, name: String) -> Result<(), AppFailure> {
            self.record(format!("rename_tab({tab_id},{name})"))
        }

        async fn new_pane(&self, floating: bool) -> Result<(), AppFailure> {
            self.record(format!("new_pane({floating})"))
        }

        async fn rename_pane(
            &self,
            pane_id: u32,
            is_plugin: bool,
            name: String,
        ) -> Result<(), AppFailure> {
            self.record(format!("rename_pane({pane_id},{is_plugin},{name})"))
        }

        async fn toggle_pane_fullscreen(
            &self,
            pane_id: u32,
            is_plugin: bool,
        ) -> Result<(), AppFailure> {
            self.record(format!("toggle_pane_fullscreen({pane_id},{is_plugin})"))
        }
    }

    fn recorded(session_id: &str) -> (Arc<TerminalController>, Arc<RecordingRepository>) {
        let repository = Arc::new(RecordingRepository::default());
        let controller = Arc::new(TerminalController::new(
            Arc::clone(&repository) as Arc<dyn TerminalRepository + Send + Sync>,
            Arc::new(MockSessionsRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            Some(session_id.to_string()),
        ));
        block_on(controller.load());
        (controller, repository)
    }

    fn focused_pane(controller: &TerminalController, key: &str) -> Option<u32> {
        controller
            .tabs
            .get_untracked()
            .into_iter()
            .find(|tab| tab.key == key)
            .and_then(|tab| tab.panes.into_iter().find(|pane| pane.focused))
            .map(|pane| pane.id)
    }

    fn docked(session_id: &str) -> (Arc<TerminalController>, Arc<RecordingRepository>) {
        let (c, repository) = recorded(session_id);
        assert!(c.set_docked(true));
        assert!(!c.set_docked(true), "only a real flip reports a change");
        block_on(c.load());
        (c, repository)
    }

    #[test]
    fn a_docked_load_rests_on_the_selected_space_and_the_active_tab() {
        let (c, _repository) = docked("herdr:work");
        assert_eq!(c.expanded_space.get_untracked().as_deref(), Some("ws-1"));
        assert_eq!(c.expanded_tab.get_untracked().as_deref(), Some("alpha"));
    }

    #[test]
    fn a_docked_rail_follows_another_clients_focus_only_while_resting() {
        let (c, repository) = docked(SEEDED_DEV_ID);
        repository.report_active("beta");
        block_on(c.reload_tabs());
        assert_eq!(c.expanded_tab.get_untracked().as_deref(), Some("beta"));

        c.collapse_tab();
        repository.report_active("alpha");
        block_on(c.reload_tabs());
        assert_eq!(
            c.expanded_tab.get_untracked(),
            None,
            "a browse is the user's"
        );
    }

    #[test]
    fn the_sheet_never_follows_a_moved_focus() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        c.seed_expansion();
        repository.report_active("beta");
        block_on(c.reload_tabs());
        assert_eq!(c.expanded_tab.get_untracked().as_deref(), Some("alpha"));
    }

    #[test]
    fn closing_a_browsed_space_docked_hands_the_rail_back_to_rest() {
        let (c, _repository) = docked("herdr:work");
        c.expanded_space.set(Some("ws-2".to_string()));
        c.expanded_tab.set(None);
        c.browsed_space_tabs
            .set(Some(("ws-2".to_string(), Vec::new())));

        c.close_space("ws-2");
        assert_eq!(c.expanded_space.get_untracked().as_deref(), Some("ws-1"));
        assert_eq!(c.expanded_tab.get_untracked().as_deref(), Some("alpha"));
        assert_eq!(c.browsed_space_tabs.get_untracked(), None);

        c.close_space("ws-1");
        assert_eq!(
            c.expanded_space.get_untracked(),
            None,
            "the rest space collapses"
        );
    }

    #[test]
    fn a_zellij_commit_inside_the_active_tab_focuses_the_pane_and_nothing_else() {
        let (c, repository) = recorded(SEEDED_DEV_ID);

        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));

        assert_eq!(repository.actions(), ["focus_pane(11,false)"]);
        assert_eq!(
            focused_pane(&c, "alpha"),
            Some(11),
            "the badge moves the instant the sheet reopens"
        );
    }

    #[test]
    fn a_zellij_commit_across_tabs_moves_the_tab_first_then_the_pane() {
        let (c, repository) = recorded(SEEDED_DEV_ID);

        assert!(block_on(c.commit_pane("beta".to_string(), 21, false)));

        assert_eq!(
            repository.actions(),
            ["go_to_tab(2)", "focus_pane(21,false)"],
            "a pane can only be focused inside the focused tab"
        );
        assert_eq!(c.active_tab.get_untracked(), "beta");
        assert_eq!(focused_pane(&c, "beta"), Some(21));
        assert_eq!(
            focused_pane(&c, "alpha"),
            Some(10),
            "the tab left behind keeps its own focus flag"
        );
    }

    #[test]
    fn a_herdr_commit_inside_the_active_tab_focuses_the_pane_on_the_live_stream() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();

        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));

        assert_eq!(
            repository.actions(),
            ["focus_pane(11,false)"],
            "the verb IS the move on herdr: the server re-points this \
             connection's relay onto that pane, over the open stream"
        );
        assert_eq!(focused_pane(&c, "alpha"), Some(11));
    }

    #[test]
    fn a_herdr_commit_across_tabs_moves_the_tab_first_then_the_pane_too() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();

        assert!(block_on(c.commit_pane("beta".to_string(), 21, false)));

        assert_eq!(
            repository.actions(),
            ["go_to_tab(2)", "focus_pane(21,false)"],
            "the same two verbs zellij takes, in the same order"
        );
        assert_eq!(c.active_tab.get_untracked(), "beta");
        assert_eq!(focused_pane(&c, "beta"), Some(21));
    }

    #[test]
    fn a_commit_re_reads_nothing_behind_it_on_either_backend() {
        for session in [SEEDED_DEV_ID, "herdr:work"] {
            let (c, repository) = recorded(session);
            c.attach_live();
            let after_load = repository.calls().len();

            assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));

            let since_load: Vec<String> = repository.calls().into_iter().skip(after_load).collect();
            assert_eq!(
                since_load,
                ["focus_pane(11,false)"],
                "the optimistic badge is the feedback and the layout poll is the \
                 truth — a reload here could only land inside the relay's own \
                 apply window and revert what the commit just wrote ({session})"
            );
        }
    }

    #[test]
    fn a_refused_tab_step_stops_the_commit_before_the_pane_focus() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("go_to_tab");

        assert!(!block_on(c.commit_pane("beta".to_string(), 21, false)));

        assert_eq!(
            repository.actions(),
            ["go_to_tab(2)"],
            "the pane focus never went out"
        );
        assert_eq!(c.active_tab.get_untracked(), "alpha", "nothing moved");
        assert_eq!(
            focused_pane(&c, "beta"),
            Some(20),
            "…and nothing was written"
        );
    }

    #[test]
    fn a_refused_pane_focus_writes_no_optimistic_state() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("focus_pane");

        assert!(!block_on(c.commit_pane("alpha".to_string(), 11, false)));

        assert_eq!(repository.actions(), ["focus_pane(11,false)"]);
        assert_eq!(
            focused_pane(&c, "alpha"),
            Some(10),
            "the badge stays on the pane the session is actually on"
        );
    }

    #[test]
    fn a_second_pane_tap_while_a_commit_is_in_flight_is_dropped_not_queued() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        repository.delay_calls([Duration::from_millis(60)]);
        let after_load = repository.calls().len();

        let (first, second) = block_on(async {
            tokio::join!(
                c.commit_pane("beta".to_string(), 21, false),
                c.commit_pane("alpha".to_string(), 11, false),
            )
        });

        assert!(first, "the tap holding the claim commits");
        assert!(!second, "…and the one that found it taken is dropped");
        let since_load: Vec<String> = repository.calls().into_iter().skip(after_load).collect();
        assert_eq!(
            since_load,
            ["go_to_tab(2)", "tab_lines(beta)", "focus_pane(21,false)"],
            "one focus_pane went out, not two racing to an order the badge \
             could not reconstruct"
        );
        assert_eq!(focused_pane(&c, "beta"), Some(21));
        assert_eq!(
            focused_pane(&c, "alpha"),
            Some(10),
            "the dropped tap wrote nothing at all"
        );
        assert_eq!(
            c.resume_slot().target(),
            ResumeTarget {
                space_id: None,
                tab_id: Some(2),
                pane_id: Some(21),
            },
            "…so the reconnect hint names the move that actually landed"
        );

        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));
        assert_eq!(focused_pane(&c, "alpha"), Some(11));
    }

    #[test]
    fn expanding_the_selected_space_performs_no_rpc_at_all() {
        let (c, repository) = recorded("herdr:work");
        let after_load = repository.calls();

        block_on(c.expand_space("ws-1".to_string()));

        assert_eq!(c.expanded_space.get_untracked(), Some("ws-1".to_string()));
        assert_eq!(
            repository.calls(),
            after_load,
            "the selected space's tabs already live in `tabs`"
        );
    }

    #[test]
    fn expanding_another_space_fetches_its_tabs_without_switching_selection() {
        let c = controller(Some("herdr:work"));
        block_on(c.load());
        let selected_before: Vec<String> = c
            .spaces
            .get_untracked()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.id)
            .collect();

        block_on(c.expand_space("zelli_mobile".to_string()));

        assert_eq!(
            c.expanded_space.get_untracked(),
            Some("zelli_mobile".to_string())
        );
        let (cached_id, cached_tabs) = c
            .browsed_space_tabs
            .get_untracked()
            .expect("the peek landed");
        assert_eq!(cached_id, "zelli_mobile");
        let keys: Vec<String> = cached_tabs.into_iter().map(|tab| tab.key).collect();
        assert_eq!(keys, ["logs", "worker"]);
        let selected_after: Vec<String> = c
            .spaces
            .get_untracked()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.id)
            .collect();
        assert_eq!(
            selected_after, selected_before,
            "browsing another space in the tree sheet must not switch to it"
        );
    }

    #[test]
    fn a_peek_that_lands_after_a_newer_expand_publishes_nothing() {
        let (c, repository) = recorded("herdr:work");
        repository.delay_calls([Duration::from_millis(60), Duration::ZERO]);

        block_on(async {
            tokio::join!(
                c.expand_space("ws-2".to_string()),
                c.expand_space("quill".to_string()),
            );
        });

        assert_eq!(
            c.expanded_space.get_untracked(),
            Some("quill".to_string()),
            "the later expand stands"
        );
        assert_eq!(
            c.browsed_space_tabs.get_untracked().map(|(id, _)| id),
            Some("quill".to_string()),
            "the earlier, now-stale peek's answer must not overwrite it"
        );
    }

    #[test]
    fn a_failed_peek_toasts_once_and_collapses_the_expanded_space() {
        let (c, repository) = recorded("herdr:work");
        repository.refuse("list_tabs");
        let failures = Arc::new(Mutex::new(0u32));
        let counted = Arc::clone(&failures);
        c.core()
            .failures()
            .subscribe(move |_: &AppFailure| {
                *counted.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            })
            .forget();

        block_on(c.expand_space("ws-2".to_string()));

        assert_eq!(
            *failures.lock().unwrap_or_else(PoisonError::into_inner),
            1,
            "exactly one toast"
        );
        assert_eq!(
            c.expanded_space.get_untracked(),
            None,
            "the row collapses back rather than showing a broken expansion"
        );
    }

    #[test]
    fn collapse_space_also_collapses_the_tab_expanded_under_it() {
        let c = controller(Some("herdr:work"));
        block_on(c.load());
        block_on(c.expand_space("zelli_mobile".to_string()));
        c.expand_tab("logs".to_string());

        c.collapse_space();

        assert_eq!(c.expanded_space.get_untracked(), None);
        assert_eq!(c.expanded_tab.get_untracked(), None);
    }

    #[test]
    fn collapse_tab_leaves_the_expanded_space_alone() {
        let c = controller(Some("herdr:work"));
        block_on(c.load());
        block_on(c.expand_space("zelli_mobile".to_string()));
        c.expand_tab("logs".to_string());

        c.collapse_tab();

        assert_eq!(
            c.expanded_space.get_untracked(),
            Some("zelli_mobile".to_string())
        );
        assert_eq!(c.expanded_tab.get_untracked(), None);
    }

    #[test]
    fn expanding_a_different_space_collapses_the_tab_expanded_under_the_last_one() {
        let c = controller(Some("herdr:work"));
        block_on(c.load());
        block_on(c.expand_space("zelli_mobile".to_string()));
        c.expand_tab("logs".to_string());

        block_on(c.expand_space("quill".to_string()));

        assert_eq!(c.expanded_space.get_untracked(), Some("quill".to_string()));
        assert_eq!(
            c.expanded_tab.get_untracked(),
            None,
            "a tab from the space just left is out of scope for the new one"
        );
    }

    #[test]
    fn seed_expansion_opens_the_selected_space_and_active_tab_on_herdr() {
        let c = controller(Some("herdr:work"));
        block_on(c.load());

        c.seed_expansion();

        assert_eq!(
            c.expanded_space.get_untracked(),
            Some("forgekit".to_string()),
            "the resting state opens on the session's own space"
        );
        assert_eq!(c.expanded_tab.get_untracked(), Some("devbox".to_string()));
    }

    #[test]
    fn seed_expansion_opens_only_the_active_tab_on_zellij() {
        let c = controller(Some(SEEDED_DEV_ID));
        block_on(c.load());

        c.seed_expansion();

        assert_eq!(
            c.expanded_space.get_untracked(),
            None,
            "zellij has no spaces — the top level is tabs"
        );
        assert_eq!(c.expanded_tab.get_untracked(), Some("devbox".to_string()));
    }

    #[test]
    fn reset_browse_clears_expansion_and_the_browsed_space_cache() {
        let c = controller(Some("herdr:work"));
        block_on(c.load());
        block_on(c.expand_space("zelli_mobile".to_string()));
        c.expand_tab("logs".to_string());

        c.reset_browse();

        assert_eq!(c.expanded_space.get_untracked(), None);
        assert_eq!(c.expanded_tab.get_untracked(), None);
        assert_eq!(c.browsed_space_tabs.get_untracked(), None);
    }

    #[test]
    fn expansion_survives_a_layout_poll_republish() {
        let (c, _repository) = recorded("herdr:work");
        block_on(c.expand_space("ws-2".to_string()));
        c.expand_tab("beta".to_string());

        block_on(c.refresh_layout_from_poll());

        assert_eq!(c.expanded_space.get_untracked(), Some("ws-2".to_string()));
        assert_eq!(c.expanded_tab.get_untracked(), Some("beta".to_string()));
    }

    #[test]
    fn a_same_space_commit_via_the_cross_space_entry_takes_commit_panes_own_path() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();

        assert!(block_on(c.commit_pane_in_space(
            "ws-1".to_string(),
            "beta".to_string(),
            21,
            false
        )));

        assert_eq!(
            repository.actions(),
            ["go_to_tab(2)", "focus_pane(21,false)"],
            "the selected space needs no switch_space — this is commit_pane's own path"
        );
        assert_eq!(c.active_tab.get_untracked(), "beta");
        assert_eq!(focused_pane(&c, "beta"), Some(21));
    }

    #[test]
    fn a_cross_space_commit_switches_space_then_moves_the_tab_then_the_pane() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        block_on(c.expand_space("ws-2".to_string()));
        let after_expand = repository.calls().len();

        assert!(block_on(c.commit_pane_in_space(
            "ws-2".to_string(),
            "beta".to_string(),
            21,
            false
        )));

        let since_expand: Vec<String> = repository.calls().into_iter().skip(after_expand).collect();
        assert_eq!(
            since_expand,
            [
                "switch_space(ws-2)",
                "go_to_tab(2)",
                "tab_lines(beta)",
                "focus_pane(21,false)",
                "list_spaces",
            ],
            "switch_space, then commit_pane's own tab-then-pane steps, and the \
             space list converging behind all of them"
        );
        assert_eq!(c.active_tab.get_untracked(), "beta");
        assert_eq!(focused_pane(&c, "beta"), Some(21));
        assert_eq!(
            c.resume_slot().target(),
            ResumeTarget {
                space_id: Some("ws-2".to_string()),
                tab_id: Some(2),
                pane_id: Some(21),
            },
            "the resume hint is written exactly as select_space's own switch writes it"
        );
    }

    #[test]
    fn a_cross_space_commit_moves_the_selected_flag_with_the_connection() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        block_on(c.expand_space("ws-2".to_string()));

        assert!(block_on(c.commit_pane_in_space(
            "ws-2".to_string(),
            "beta".to_string(),
            21,
            false
        )));

        assert_eq!(
            c.selected_space_id().as_deref(),
            Some("ws-2"),
            "the sheet's own space list must name the space the connection is \
             now on — every later open seeds its card from this flag"
        );
        c.seed_expansion();
        assert_eq!(c.expanded_space.get_untracked(), Some("ws-2".to_string()));

        let after_first = repository.calls().len();
        assert!(block_on(c.commit_pane_in_space(
            "ws-2".to_string(),
            "alpha".to_string(),
            11,
            false
        )));
        let since_first: Vec<String> = repository.calls().into_iter().skip(after_first).collect();
        assert!(
            !since_first
                .iter()
                .any(|call| call.starts_with("switch_space")),
            "the second commit is inside the selected space: {since_first:?}"
        );
    }

    #[test]
    fn a_browse_reset_racing_a_cross_space_commit_fails_it_closed() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        block_on(c.expand_space("ws-2".to_string()));
        let after_expand = repository.calls().len();
        c.reset_browse();

        assert!(!block_on(c.commit_pane_in_space(
            "ws-2".to_string(),
            "beta".to_string(),
            21,
            false
        )));

        let since_expand: Vec<String> = repository.calls().into_iter().skip(after_expand).collect();
        assert!(
            since_expand.is_empty(),
            "no switch_space went out at all — a half-landed switch would strand \
             the session in a space whose tabs this commit can no longer resolve \
             against: {since_expand:?}"
        );
        assert_eq!(
            c.selected_space_id().as_deref(),
            Some("ws-1"),
            "…and the connection stayed where it was"
        );
    }

    #[test]
    fn a_refused_cross_space_switch_never_moves_the_tab_or_pane() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        repository.refuse("switch_space");
        block_on(c.expand_space("ws-2".to_string()));
        let after_expand = repository.calls().len();

        assert!(!block_on(c.commit_pane_in_space(
            "ws-2".to_string(),
            "beta".to_string(),
            21,
            false
        )));

        let since_expand: Vec<String> = repository.calls().into_iter().skip(after_expand).collect();
        assert_eq!(
            since_expand,
            ["switch_space(ws-2)"],
            "nothing past the refusal"
        );
        assert_eq!(
            c.resume_slot().target(),
            ResumeTarget::default(),
            "a switch the server refused never happened"
        );
    }

    #[test]
    fn a_layout_fetch_issued_before_a_cross_space_commit_cannot_revert_its_tab_world() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        repository.stamp_lists();
        block_on(c.expand_space("ws-2".to_string()));
        repository.delay_calls([Duration::from_millis(60)]);

        block_on(async {
            tokio::join!(
                c.refresh_layout_from_poll(),
                c.commit_pane_in_space("ws-2".to_string(), "beta".to_string(), 21, false),
            );
        });

        assert_eq!(
            labels(&c),
            ["alpha#2", "beta#2"],
            "the browse's own promoted answer stands — the stamp names the round \
             trip that published, and the poll's later one is outdated by the \
             promotion rather than healing the tabs back to the space just left"
        );
        assert_eq!(
            c.selected_space_id().as_deref(),
            Some("ws-2"),
            "…so the space list and the tab world still name the same space"
        );
    }

    #[test]
    fn a_commit_racing_a_space_switch_finds_the_same_claim_taken() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        repository.delay_calls([Duration::from_millis(60)]);
        let after_load = repository.calls().len();

        let (adopted, committed) = block_on(async {
            tokio::join!(
                c.adopt_space("ws-2".to_string()),
                c.commit_pane("alpha".to_string(), 11, false),
            )
        });

        assert!(adopted, "the switch holding the claim runs");
        assert!(!committed, "…and the commit that found it taken is dropped");
        let since_load: Vec<String> = repository.calls().into_iter().skip(after_load).collect();
        assert!(
            !since_load.iter().any(|call| call.starts_with("focus_pane")),
            "a commit and a switch move the same two signals, so the loser sends \
             nothing rather than focusing a pane in whichever space the switch \
             leaves it in: {since_load:?}"
        );

        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));
        assert_eq!(focused_pane(&c, "alpha"), Some(11));
    }

    #[test]
    fn a_grid_tap_focus_never_switches_tabs_and_reloads_after() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        let after_load = repository.calls();

        assert!(block_on(c.focus_pane_from_tap(11, false)));

        let since_load: Vec<String> = repository
            .calls()
            .into_iter()
            .skip(after_load.len())
            .collect();
        assert_eq!(
            since_load,
            ["focus_pane(11,false)", "list_tabs"],
            "no go_to_tab at all — a grid tap never crosses the SERVER-ACTIVE \
             tab — and a reload lands after the focus, unlike commit_pane's \
             own optimistic-only story"
        );
    }

    #[test]
    fn a_refused_grid_tap_focus_writes_no_optimistic_state_and_reloads_nothing() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("focus_pane");

        assert!(!block_on(c.focus_pane_from_tap(11, false)));

        assert_eq!(repository.actions(), ["focus_pane(11,false)"]);
        assert_eq!(
            focused_pane(&c, "alpha"),
            Some(10),
            "nothing moved, and nothing was reloaded either"
        );
    }

    #[test]
    fn a_wheel_scroll_fallback_calls_scroll_pane_and_ignores_its_answer() {
        let (c, repository) = recorded(SEEDED_DEV_ID);

        block_on(c.scroll_pane_from_wheel(21, false, ScrollStep::Up));

        assert_eq!(repository.actions(), ["scroll_pane(21,false,Up)"]);
    }

    #[test]
    fn a_refused_wheel_scroll_fallback_is_silently_dropped() {
        let (c, repository) = recorded("herdr:work");
        repository.refuse("scroll_pane");

        block_on(c.scroll_pane_from_wheel(21, false, ScrollStep::Down));

        assert_eq!(repository.actions(), ["scroll_pane(21,false,Down)"]);
    }

    fn pane_fullscreen(controller: &TerminalController, id: u32, is_plugin: bool) -> Option<bool> {
        controller
            .tabs
            .get_untracked()
            .into_iter()
            .flat_map(|tab| tab.panes)
            .find(|pane| pane.id == id && pane.is_plugin == is_plugin)
            .map(|pane| pane.is_fullscreen)
    }

    #[test]
    fn toggle_pane_fullscreen_from_pinch_is_refused_on_herdr_before_any_rpc() {
        let (c, repository) = recorded("herdr:work");
        let after_load = repository.calls().len();

        assert!(!block_on(c.toggle_pane_fullscreen_from_pinch(11, false)));

        assert_eq!(
            repository.calls().len(),
            after_load,
            "a herdr view already shows one pane — no verb, and no reload behind it"
        );
    }

    #[test]
    fn toggle_pane_fullscreen_from_pinch_toggles_and_reloads_the_layout_on_zellij() {
        let c = controller(Some(SEEDED_DEV_ID));
        block_on(c.load());
        assert_eq!(pane_fullscreen(&c, 11, false), Some(false));
        let writes_before = c.layout_writes();

        assert!(block_on(c.toggle_pane_fullscreen_from_pinch(11, false)));

        assert_eq!(pane_fullscreen(&c, 11, false), Some(true));
        assert!(
            c.layout_writes() > writes_before,
            "a landed toggle reloads the tabs so the flag converges now"
        );
    }

    #[test]
    fn toggle_pane_fullscreen_from_pinch_is_single_flight_under_an_overlapping_call() {
        let c = controller(Some(SEEDED_DEV_ID));
        block_on(c.load());

        let (first, second) = block_on(async {
            tokio::join!(
                c.toggle_pane_fullscreen_from_pinch(11, false),
                c.toggle_pane_fullscreen_from_pinch(11, false),
            )
        });

        assert!(
            first ^ second,
            "exactly one of the two overlapping calls lands"
        );
        assert_eq!(
            pane_fullscreen(&c, 11, false),
            Some(true),
            "one flip, not two — the refused call sent no RPC"
        );
        assert!(
            block_on(c.toggle_pane_fullscreen_from_pinch(11, false)),
            "the claim releases once the first call has finished"
        );
        assert_eq!(pane_fullscreen(&c, 11, false), Some(false));
    }

    #[test]
    fn toggle_pane_fullscreen_from_pinch_surfaces_a_refused_ack_on_the_failure_sink() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("toggle_pane_fullscreen");
        let failures = Arc::new(Mutex::new(0u32));
        let counted = Arc::clone(&failures);
        c.core()
            .failures()
            .subscribe(move |_: &AppFailure| {
                *counted.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            })
            .forget();
        let after_load = repository.calls().len();

        assert!(!block_on(c.toggle_pane_fullscreen_from_pinch(11, false)));

        let calls = repository.calls();
        assert_eq!(&calls[after_load..], ["toggle_pane_fullscreen(11,false)"]);
        assert_eq!(*failures.lock().unwrap_or_else(PoisonError::into_inner), 1);
    }

    #[test]
    fn a_space_switch_reloads_the_tab_world_behind_it() {
        let (c, repository) = recorded("herdr:work");

        block_on(c.select_space("ws-2".to_string()));

        let calls = repository.calls();
        let after_switch: Vec<String> = calls
            .iter()
            .skip_while(|call| *call != "switch_space(ws-2)")
            .cloned()
            .collect();
        assert_eq!(
            after_switch,
            [
                "switch_space(ws-2)",
                "list_spaces",
                "list_tabs",
                "tab_lines(alpha)"
            ],
            "a space is a different set of tabs, so the switch re-reads them"
        );
        assert_eq!(
            c.active_tab.get_untracked(),
            "alpha",
            "…and the sheet lands on the new space's own active tab"
        );
    }

    #[test]
    fn a_refused_space_switch_reloads_nothing() {
        let (c, repository) = recorded("herdr:work");
        repository.refuse("switch_space");
        let before = repository.calls().len();

        block_on(c.select_space("ws-2".to_string()));

        assert_eq!(
            repository.calls().len(),
            before + 1,
            "the refused switch, and no reload behind it"
        );
        assert!(c.loaded.get_untracked());
    }

    #[test]
    fn a_herdr_commit_records_the_move_as_the_reconnects_resume_hint() {
        let (c, _repository) = recorded("herdr:work");
        c.attach_live();

        assert!(block_on(c.commit_pane("beta".to_string(), 21, false)));

        assert_eq!(
            c.resume_slot().target(),
            ResumeTarget {
                space_id: None,
                tab_id: Some(2),
                pane_id: Some(21),
            },
            "a reconnect must ask for the pane the user chose, not the one the \
             daemon has globally focused"
        );
    }

    #[test]
    fn a_space_switch_records_the_space_and_a_commit_after_it_keeps_it() {
        let (c, _repository) = recorded("herdr:work");
        c.attach_live();

        block_on(c.select_space("ws-2".to_string()));

        assert_eq!(
            c.resume_slot().target(),
            ResumeTarget {
                space_id: Some("ws-2".to_string()),
                tab_id: None,
                pane_id: None,
            },
            "the tab and pane hints named the space just left"
        );

        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));

        assert_eq!(
            c.resume_slot().target(),
            ResumeTarget {
                space_id: Some("ws-2".to_string()),
                tab_id: Some(1),
                pane_id: Some(11),
            },
            "the space rides along with every later move — it is the rung a \
             muxrd restart cannot stale"
        );
    }

    #[test]
    fn a_refused_move_records_no_resume_hint_at_all() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        repository.refuse("switch_space");
        block_on(c.select_space("ws-2".to_string()));

        repository.refuse("focus_pane");
        assert!(!block_on(c.commit_pane("alpha".to_string(), 11, false)));

        assert_eq!(
            c.resume_slot().target(),
            ResumeTarget::default(),
            "a move the server refused never happened, so there is nothing to \
             resume onto"
        );
    }

    #[test]
    fn a_zellij_commit_records_no_resume_hint() {
        let (c, _repository) = recorded(SEEDED_DEV_ID);

        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));

        assert_eq!(
            c.resume_slot().target(),
            ResumeTarget::default(),
            "a zellij stream is the whole composited session and follows the \
             server's own focus: its attach carries no resume fields, ever"
        );
    }

    #[test]
    fn a_layout_read_never_writes_the_resume_hint() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));
        let recorded_move = c.resume_slot().target();

        repository.report_focus((10, false));
        block_on(c.reload_tabs());
        block_on(c.refresh_layout_from_poll());

        assert_eq!(
            focused_pane(&c, "alpha"),
            Some(10),
            "the poll really did answer with a different pane"
        );
        assert_eq!(
            c.resume_slot().target(),
            recorded_move,
            "…and the hint still names the pane the USER chose — recording a \
             poll's answer would overwrite the very intent it exists to replay"
        );
    }

    #[test]
    fn creating_a_space_reloads_the_list_and_selects_it_by_label() {
        let (c, repository) = recorded("herdr:work");

        block_on(c.create_space("two".to_string()));

        assert_eq!(
            repository.actions(),
            ["create_space(two)"],
            "the reload is a direct list_spaces, filtered out of `actions`"
        );
        let selected: Vec<String> = c
            .spaces
            .get_untracked()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.name)
            .collect();
        assert_eq!(selected, ["two"], "the freshly listed match is selected");
    }

    #[test]
    fn a_create_with_no_matching_name_leaves_the_reloaded_selection_alone() {
        let (c, _repository) = recorded("herdr:work");

        block_on(c.create_space("nope".to_string()));

        let selected: Vec<String> = c
            .spaces
            .get_untracked()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.name)
            .collect();
        assert_eq!(selected, ["one"], "whatever the server answered stands");
    }

    #[test]
    fn a_refused_space_create_reports_the_failure_and_reloads_nothing() {
        let (c, repository) = recorded("herdr:work");
        repository.refuse("create_space");
        let before = c.spaces.get_untracked();

        block_on(c.create_space("nope".to_string()));

        assert_eq!(
            repository.calls().last(),
            Some(&"create_space(nope)".to_string()),
            "the refused create is the last call — no reload behind it"
        );
        assert_eq!(c.spaces.get_untracked(), before, "nothing was written");
    }

    #[test]
    fn a_new_tab_carries_its_dialog_name_through_and_reloads_the_workspace() {
        let (c, repository) = recorded(SEEDED_DEV_ID);

        block_on(c.new_tab("build".to_string()));

        assert_eq!(
            repository.actions(),
            ["new_tab(build)"],
            "one RPC, carrying the dialog's own name — the reload behind it \
             is reads, filtered out of `actions`"
        );
        assert_eq!(
            c.active_tab.get_untracked(),
            "alpha",
            "the reload re-seeded the active tab onto the server-active one"
        );
    }

    #[test]
    fn a_blank_dialog_confirm_creates_an_unnamed_tab() {
        let (c, repository) = recorded(SEEDED_DEV_ID);

        block_on(c.new_tab(String::new()));

        assert_eq!(
            repository.actions(),
            ["new_tab()"],
            "an empty name rides straight through — the wire's own default \
             naming, not a client-side substitution"
        );
    }

    #[test]
    fn a_refused_new_tab_reloads_nothing() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("new_tab");
        let tabs = c.tabs.get_untracked();

        block_on(c.new_tab("build".to_string()));

        assert_eq!(repository.actions(), ["new_tab(build)"]);
        assert_eq!(c.tabs.get_untracked(), tabs, "no reload behind the refusal");
    }

    #[test]
    fn a_pane_tap_opens_a_tiled_pane_and_a_long_press_opens_a_floating_one() {
        let (c, repository) = recorded(SEEDED_DEV_ID);

        block_on(c.new_pane(false));
        block_on(c.new_pane(true));

        assert_eq!(
            repository.actions(),
            ["new_pane(false)", "new_pane(true)"],
            "the tap/long-press flag rides straight through to the verb"
        );
    }

    #[test]
    fn a_refused_new_pane_reloads_nothing() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("new_pane");
        let tabs = c.tabs.get_untracked();

        block_on(c.new_pane(false));

        assert_eq!(repository.actions(), ["new_pane(false)"]);
        assert_eq!(c.tabs.get_untracked(), tabs, "no reload behind the refusal");
    }

    #[test]
    fn an_add_tab_row_under_a_browsed_space_switches_to_it_before_creating() {
        let (c, repository) = recorded("herdr:work");
        block_on(c.expand_space("ws-2".to_string()));
        let after_expand = repository.calls().len();

        assert!(block_on(c.adopt_space("ws-2".to_string())));
        block_on(c.new_tab("logs".to_string()));

        let since_expand: Vec<String> = repository.calls().into_iter().skip(after_expand).collect();
        let switched = since_expand
            .iter()
            .position(|call| call == "switch_space(ws-2)")
            .expect("the browsed space was switched to");
        let created = since_expand
            .iter()
            .position(|call| call == "new_tab(logs)")
            .expect("…and the tab was created");
        assert!(
            switched < created,
            "the switch has to precede the create or the tab lands in the space \
             the user was only looking away from: {since_expand:?}"
        );
        assert_eq!(c.selected_space_id().as_deref(), Some("ws-2"));
    }

    #[test]
    fn an_add_row_under_the_selected_space_switches_nothing() {
        let (c, repository) = recorded("herdr:work");
        let after_load = repository.calls().len();

        assert!(block_on(c.adopt_space("ws-1".to_string())));

        let since_load: Vec<String> = repository.calls().into_iter().skip(after_load).collect();
        assert!(
            since_load.is_empty(),
            "already there — no RPC at all: {since_load:?}"
        );
    }

    #[test]
    fn an_add_pane_row_moves_to_its_own_space_and_tab_before_creating() {
        let (c, repository) = recorded("herdr:work");
        block_on(c.expand_space("ws-2".to_string()));
        let after_expand = repository.calls().len();

        block_on(c.new_pane_in(Some("ws-2".to_string()), "beta".to_string(), false));

        let since_expand: Vec<String> = repository.calls().into_iter().skip(after_expand).collect();
        let moves: Vec<&String> = since_expand
            .iter()
            .filter(|call| {
                call.starts_with("switch_space")
                    || call.starts_with("go_to_tab")
                    || call.starts_with("new_pane")
            })
            .collect();
        assert_eq!(
            moves,
            ["switch_space(ws-2)", "go_to_tab(2)", "new_pane(false)"],
            "space, then tab, then the create — a pane only opens in the tab \
             the session is on"
        );
    }

    #[test]
    fn an_add_pane_row_under_the_tab_the_session_is_on_creates_straight_away() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        let after_load = repository.calls().len();

        block_on(c.new_pane_in(None, "alpha".to_string(), false));

        let since_load: Vec<String> = repository.calls().into_iter().skip(after_load).collect();
        assert!(
            !since_load.iter().any(|call| call.starts_with("go_to_tab")),
            "nothing to move: {since_load:?}"
        );
        assert_eq!(
            since_load.first().map(String::as_str),
            Some("new_pane(false)")
        );
    }

    #[test]
    fn a_second_add_pane_tap_racing_the_first_ones_switch_sends_nothing_at_all() {
        let _override = SettleOverride::install(Duration::ZERO);
        let (c, repository) = recorded("herdr:work");
        block_on(c.expand_space("ws-2".to_string()));
        repository.delay_calls([Duration::from_millis(60)]);
        let after_expand = repository.calls().len();

        block_on(async {
            tokio::join!(
                c.new_pane_in(Some("ws-2".to_string()), "beta".to_string(), false),
                c.new_pane_in(Some("ws-1".to_string()), "alpha".to_string(), false),
            );
        });

        let since_expand: Vec<String> = repository.calls().into_iter().skip(after_expand).collect();
        let moves: Vec<&String> = since_expand
            .iter()
            .filter(|call| call.starts_with("switch_space") || call.starts_with("new_pane"))
            .collect();
        assert_eq!(
            moves,
            ["switch_space(ws-2)", "new_pane(false)"],
            "exactly one row drove the session: the tap that lost the claim \
             created nothing AND switched nothing — a switch of its own would \
             have moved the session out from under the pane the winner opened, \
             with no toast to say so: {since_expand:?}"
        );
        assert_eq!(
            c.selected_space_id().as_deref(),
            Some("ws-2"),
            "…so the session is on the space whose row actually created"
        );
    }

    #[test]
    fn a_refused_switch_under_an_add_pane_row_creates_nothing() {
        let (c, repository) = recorded("herdr:work");
        block_on(c.expand_space("ws-2".to_string()));
        repository.refuse("switch_space");
        let after_expand = repository.calls().len();

        block_on(c.new_pane_in(Some("ws-2".to_string()), "beta".to_string(), false));

        let since_expand: Vec<String> = repository.calls().into_iter().skip(after_expand).collect();
        assert_eq!(
            since_expand,
            ["switch_space(ws-2)"],
            "a pane must never land in the space the switch failed to leave"
        );
    }

    #[test]
    fn renaming_a_space_reloads_the_list_and_writes_no_reveal() {
        let (c, repository) = recorded("herdr:work");
        let epoch = c.reveal_epoch.get_untracked();

        block_on(c.rename_space("ws-1".to_string(), "renamed".to_string()));

        assert_eq!(
            repository.actions(),
            ["rename_space(ws-1,renamed)"],
            "the reload is a direct list_spaces, filtered out of `actions`"
        );
        assert_eq!(
            c.reveal_epoch.get_untracked(),
            epoch,
            "renaming a space touches no tab, so no replay"
        );
    }

    #[test]
    fn a_refused_space_rename_reloads_nothing() {
        let (c, repository) = recorded("herdr:work");
        repository.refuse("rename_space");
        let before = c.spaces.get_untracked();

        block_on(c.rename_space("ws-1".to_string(), "renamed".to_string()));

        assert_eq!(
            repository.calls().last(),
            Some(&"rename_space(ws-1,renamed)".to_string()),
            "the refused rename is the last call — no reload behind it"
        );
        assert_eq!(c.spaces.get_untracked(), before, "nothing was written");
    }

    #[test]
    fn renaming_a_tab_reloads_only_the_tab_list() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        let active = c.active_tab.get_untracked();
        let epoch = c.reveal_epoch.get_untracked();

        block_on(c.rename_tab(1, "renamed".to_string()));

        assert_eq!(
            repository.actions(),
            ["rename_tab(1,renamed)"],
            "the reload is a direct list_tabs, filtered out of `actions`"
        );
        assert_eq!(c.active_tab.get_untracked(), active, "no tab moved");
        assert_eq!(c.reveal_epoch.get_untracked(), epoch, "…and no replay");
    }

    #[test]
    fn a_refused_tab_rename_reloads_nothing() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("rename_tab");
        let tabs = c.tabs.get_untracked();

        block_on(c.rename_tab(1, "renamed".to_string()));

        assert_eq!(repository.actions(), ["rename_tab(1,renamed)"]);
        assert_eq!(c.tabs.get_untracked(), tabs, "no reload behind the refusal");
    }

    #[test]
    fn renaming_a_pane_reloads_the_tab_list_too() {
        let (c, repository) = recorded(SEEDED_DEV_ID);

        block_on(c.rename_pane(10, false, "renamed".to_string()));

        assert_eq!(
            repository.actions(),
            ["rename_pane(10,false,renamed)"],
            "a pane's title lives inside TabEntry::panes, so the same reload carries it"
        );
    }

    #[test]
    fn a_refused_pane_rename_reloads_nothing() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("rename_pane");
        let tabs = c.tabs.get_untracked();

        block_on(c.rename_pane(10, false, "renamed".to_string()));

        assert_eq!(repository.actions(), ["rename_pane(10,false,renamed)"]);
        assert_eq!(c.tabs.get_untracked(), tabs, "no reload behind the refusal");
    }

    /// Serializes the tests installing the process-global settle override.
    static SETTLE_OVERRIDE: Mutex<()> = Mutex::new(());

    struct SettleOverride(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl SettleOverride {
        fn install(settle: Duration) -> Self {
            let guard = SETTLE_OVERRIDE
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            set_create_settle_for_tests(Some(settle));
            Self(guard)
        }
    }

    impl Drop for SettleOverride {
        fn drop(&mut self) {
            set_create_settle_for_tests(None);
        }
    }

    fn labels(controller: &TerminalController) -> Vec<String> {
        controller
            .tabs
            .get_untracked()
            .into_iter()
            .map(|tab| tab.label)
            .collect()
    }

    #[test]
    fn an_earlier_layout_fetch_landing_late_never_clobbers_the_fresher_one() {
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.stamp_lists();
        repository.delay_calls([Duration::from_millis(60), Duration::ZERO]);

        block_on(async {
            tokio::join!(c.reload_tabs(), c.reload_tabs());
        });

        assert_eq!(
            labels(&c),
            ["alpha#3", "beta#3"],
            "the newest fetch's answer stands; the older one's is dropped"
        );
    }

    #[test]
    fn a_reload_tabs_that_lands_before_load_never_drops_loads_spaces() {
        let repository = Arc::new(RecordingRepository::default());
        let c = TerminalController::new(
            Arc::clone(&repository) as Arc<dyn TerminalRepository + Send + Sync>,
            Arc::new(MockSessionsRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            Some("herdr:work".to_string()),
        );
        repository.delay_calls([Duration::from_millis(60)]);

        block_on(async {
            tokio::join!(c.load(), c.reload_tabs());
        });

        assert!(c.loaded.get_untracked());
        assert!(
            !c.spaces.get_untracked().is_empty(),
            "load()'s spaces payload must survive losing the tabs generation \
             to reload_tabs()"
        );
    }

    #[test]
    fn a_layout_poll_tick_needs_a_landed_load_and_never_stacks_two_reloads() {
        let c = controller(Some(SEEDED_DEV_ID));
        assert!(
            !c.take_layout_poll_tick(),
            "no poll before the first load has landed"
        );

        block_on(c.load());
        assert!(c.take_layout_poll_tick());
        assert!(
            !c.take_layout_poll_tick(),
            "the previous tick is still outstanding"
        );

        block_on(c.refresh_layout_from_poll());
        assert!(c.take_layout_poll_tick(), "…and finishing re-arms it");
    }

    #[test]
    fn a_poll_tick_whose_answer_has_not_moved_writes_nothing() {
        let (c, _repository) = recorded(SEEDED_DEV_ID);
        let after_load = c.layout_writes();

        block_on(c.refresh_layout_from_poll());
        block_on(c.refresh_layout_from_poll());

        assert_eq!(
            c.layout_writes(),
            after_load,
            "an unchanged tab world must wake no rebuild — the poll runs for \
             the life of the mount"
        );
    }

    #[test]
    fn a_reload_heals_an_empty_active_tab_snapshot() {
        let (c, _repository) = recorded(SEEDED_DEV_ID);

        c.active_tab.set(String::new());
        block_on(c.reload_tabs());
        assert_eq!(c.active_tab.get_untracked(), "alpha");
    }

    #[test]
    fn a_landed_create_waits_out_the_settle_before_reading_back_what_it_made() {
        const SETTLE: Duration = Duration::from_millis(80);
        let _override = SettleOverride::install(SETTLE);
        let (c, repository) = recorded(SEEDED_DEV_ID);

        let started = std::time::Instant::now();
        block_on(c.new_tab(String::new()));

        assert!(
            started.elapsed() >= SETTLE,
            "the reload behind a create waits for the relay to apply it, \
             rather than reading back the world the ack was enqueued against"
        );
        assert!(
            c.tabs.get_untracked().iter().any(|tab| tab.key == "gamma"),
            "…and then reads the tab the create actually made"
        );
        assert_eq!(repository.actions(), ["new_tab()"], "one create, not two");
        assert_eq!(
            CREATE_SETTLE.as_millis(),
            350,
            "the shipped settle is the original client's own"
        );
    }

    #[test]
    fn a_second_create_press_while_one_is_in_flight_is_dropped_not_queued() {
        let _override = SettleOverride::install(Duration::ZERO);
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.delay_calls([Duration::from_millis(60)]);

        block_on(async {
            tokio::join!(c.new_tab(String::new()), c.new_tab(String::new()));
        });

        assert_eq!(
            repository.actions(),
            ["new_tab()"],
            "a repeated press is dropped, never queued behind the first"
        );

        block_on(c.new_tab(String::new()));
        assert_eq!(repository.actions(), ["new_tab()", "new_tab()"]);
    }

    #[test]
    fn a_refused_create_re_arms_the_affordance_it_claimed() {
        let _override = SettleOverride::install(Duration::ZERO);
        let (c, repository) = recorded(SEEDED_DEV_ID);
        repository.refuse("new_pane");

        block_on(c.new_pane(false));
        block_on(c.new_pane(false));

        assert_eq!(
            repository.actions(),
            ["new_pane(false)", "new_pane(false)"],
            "nothing landed, so nothing is holding the claim"
        );
    }

    #[test]
    fn the_attach_handshake_settles_exactly_once_per_mount() {
        let c = controller(Some(SEEDED_DEV_ID));
        assert!(
            !c.mirror_link(LinkStatus::Idle, None),
            "nothing has connected yet"
        );
        assert!(!c.mirror_link(LinkStatus::Reconnecting, None));

        assert!(
            c.mirror_link(LinkStatus::Connected, Some("conn-1".to_string())),
            "the stream minted an id: the layout read that raced it needs \
             doing again"
        );
        assert!(
            !c.mirror_link(LinkStatus::Connected, Some("conn-1".to_string())),
            "…and it is said once"
        );
        assert!(
            !c.mirror_link(LinkStatus::Connected, Some("conn-2".to_string())),
            "a re-attach re-binds the repository but does not re-settle"
        );
    }

    #[test]
    fn a_connected_status_settles_the_attach_even_with_no_id_to_show_for_it() {
        let c = controller(Some(SEEDED_DEV_ID));
        assert!(c.mirror_link(LinkStatus::Connected, None));
        assert!(!c.mirror_link(LinkStatus::Connected, None));
    }

    /// Deep-linked to `beta`'s unfocused pane 21 (position 1), with nothing loaded yet.
    fn live_deep_link() -> (Arc<TerminalController>, Arc<RecordingRepository>) {
        let repository = Arc::new(RecordingRepository::default());
        let c = Arc::new(TerminalController::new(
            Arc::clone(&repository) as Arc<dyn TerminalRepository + Send + Sync>,
            Arc::new(MockSessionsRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            Some(SEEDED_DEV_ID.to_string()),
        ));
        c.set_attach_target(Some(1), Some(21));
        c.attach_live();
        (c, repository)
    }

    const DEEP_LINK: [&str; 2] = ["go_to_tab(2)", "focus_pane(21,false)"];

    #[test]
    fn a_live_deep_link_waits_for_a_list_published_after_the_settle() {
        let (c, repository) = live_deep_link();
        block_on(c.load());
        assert!(c.mirror_link(LinkStatus::Connected, Some("conn-1".to_string())));
        block_on(c.apply_attach_target());
        assert!(
            repository.actions().is_empty(),
            "a pre-handshake list never counts"
        );
        assert!(c.pending_attach_target_for_tests().is_some());

        block_on(c.reload_tabs());
        assert_eq!(repository.actions(), DEEP_LINK);
        block_on(c.reload_tabs());
        block_on(c.apply_attach_target());
        assert_eq!(repository.actions(), DEEP_LINK, "applied exactly once");
    }

    #[test]
    fn a_held_move_claim_leaves_the_deep_link_for_the_next_publish() {
        let (c, repository) = live_deep_link();
        block_on(c.load());
        c.mirror_link(LinkStatus::Connected, Some("conn-1".to_string()));
        let held = c.claim_move();
        block_on(c.reload_tabs());
        assert!(repository.actions().is_empty());
        assert!(
            c.pending_attach_target_for_tests().is_some(),
            "not lost to the refusal"
        );

        drop(held);
        block_on(c.reload_tabs());
        assert_eq!(repository.actions(), DEEP_LINK);
    }

    #[test]
    fn a_user_move_or_create_supersedes_the_deep_link() {
        let (c, repository) = live_deep_link();
        block_on(c.load());
        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));
        assert_eq!(c.pending_attach_target_for_tests(), None);
        c.mirror_link(LinkStatus::Connected, Some("conn-1".to_string()));
        block_on(c.reload_tabs());
        assert_eq!(repository.actions(), ["focus_pane(11,false)"]);

        let _override = SettleOverride::install(Duration::ZERO);
        let (c, _repository) = live_deep_link();
        block_on(c.new_tab(String::new()));
        assert_eq!(
            c.pending_attach_target_for_tests(),
            None,
            "a create moves the focus too"
        );
    }

    #[test]
    fn a_refused_move_and_a_landed_space_create_keep_the_deep_link() {
        let (c, repository) = live_deep_link();
        block_on(c.load());
        repository.refuse("focus_pane");
        assert!(!block_on(c.commit_pane("alpha".to_string(), 11, false)));
        assert!(c.pending_attach_target_for_tests().is_some());

        let _override = SettleOverride::install(Duration::ZERO);
        block_on(c.create_space("scratch".to_string()));
        assert!(
            repository
                .actions()
                .contains(&"create_space(scratch)".to_string())
        );
        assert!(
            c.pending_attach_target_for_tests().is_some(),
            "a space create moves no focus"
        );
    }

    #[test]
    fn the_live_pane_seeds_the_shipped_appearance_defaults() {
        let c = controller(None);
        assert_eq!(c.cursor_style.get_untracked(), term::CursorStyle::Block);
        assert_eq!(c.font_size.get_untracked(), DEFAULT_LIVE_FONT_SIZE);
        assert_eq!(
            c.font_family.get_untracked(),
            fonts::default_terminal_family()
        );
    }

    #[test]
    fn every_settings_cursor_style_maps_to_its_term_counterpart() {
        assert_eq!(
            map_cursor_style(SettingsCursorStyle::Block),
            term::CursorStyle::Block
        );
        assert_eq!(
            map_cursor_style(SettingsCursorStyle::Underline),
            term::CursorStyle::Underline
        );
        assert_eq!(
            map_cursor_style(SettingsCursorStyle::Bar),
            term::CursorStyle::Bar
        );
    }

    fn controller_over(prefs: &Arc<MockPreferencesRepository>) -> TerminalController {
        TerminalController::new(
            Arc::new(MockTerminalRepository::seeded()),
            Arc::new(MockSessionsRepository::seeded()),
            Arc::clone(prefs) as Arc<dyn PreferencesRepository + Send + Sync>,
            None,
        )
    }

    #[test]
    fn refresh_appearance_resolves_an_installed_font_family_with_the_symbols_fallback() {
        let prefs = Arc::new(MockPreferencesRepository::seeded());
        let c = controller_over(&prefs);
        let _ = block_on(prefs.save_appearance(AppearancePrefs {
            font_family: fonts::DEFAULT_TERMINAL_FAMILY.to_string(),
            font_size_px: 12,
            cursor_style: SettingsCursorStyle::Block,
        }));

        block_on(c.refresh_appearance());

        assert_eq!(
            c.font_family.get_untracked(),
            FontFamily::stack_with_generic(
                [
                    fonts::DEFAULT_TERMINAL_FAMILY,
                    fonts::SYMBOLS_FALLBACK_FAMILY
                ],
                frust::authoring::text::GenericSlot::Monospace,
            ),
            "an installed family names itself as the primary, trailed by the \
             Symbols fallback"
        );
    }

    #[test]
    fn refresh_appearance_falls_back_to_the_default_stack_for_an_unknown_family() {
        let prefs = Arc::new(MockPreferencesRepository::seeded());
        let c = controller_over(&prefs);
        let _ = block_on(prefs.save_appearance(AppearancePrefs {
            font_family: "Space Mono".to_string(),
            font_size_px: 12,
            cursor_style: SettingsCursorStyle::Block,
        }));

        block_on(c.refresh_appearance());

        assert_eq!(
            c.font_family.get_untracked(),
            fonts::default_terminal_family()
        );
    }

    #[test]
    fn a_second_identical_poll_writes_the_font_family_signal_nothing_further() {
        let prefs = Arc::new(MockPreferencesRepository::seeded());
        let c = controller_over(&prefs);
        let _ = block_on(prefs.save_appearance(AppearancePrefs {
            font_family: "Space Mono".to_string(),
            font_size_px: 12,
            cursor_style: SettingsCursorStyle::Block,
        }));

        block_on(c.refresh_appearance());
        let resolved = c.font_family.get_untracked();
        let after_first = c.appearance_writes();

        block_on(c.refresh_appearance());

        assert_eq!(c.font_family.get_untracked(), resolved);
        assert_eq!(
            c.appearance_writes(),
            after_first,
            "an idle poll over an unchanged font_family pref must write nothing"
        );
    }

    fn agent(pane_id: u32, tab: &str, space: &str, status: AgentStatus, seq: u64) -> AgentEntry {
        AgentEntry {
            pane_id,
            tab_id: u64::from(pane_id / 10),
            tab_key: tab.to_string(),
            space_id: Some(space.to_string()),
            space_name: Some(space.to_string()),
            tab_name: tab.to_string(),
            kind: "claude".to_string(),
            name: None,
            display_kind: None,
            status,
            title: None,
            summary: None,
            cwd: None,
            focused: false,
            change_seq: seq,
        }
    }

    fn stage_agents(repository: &RecordingRepository, agents: Vec<AgentEntry>) {
        *repository
            .agents
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = agents;
    }

    #[test]
    fn a_reload_publishes_the_agents_most_urgent_first_and_flags_a_blocked_one() {
        let (c, repository) = recorded("herdr:work");
        assert!(c.agents.get_untracked().is_empty());
        stage_agents(
            &repository,
            vec![
                agent(10, "alpha", "ws-1", AgentStatus::Idle, 9),
                agent(11, "alpha", "ws-1", AgentStatus::Working, 5),
                agent(20, "beta", "ws-1", AgentStatus::Blocked, 1),
                agent(21, "beta", "ws-1", AgentStatus::Working, 7),
            ],
        );
        block_on(c.reload_tabs());
        let order: Vec<u32> = c.agents.get_untracked().iter().map(|a| a.pane_id).collect();
        assert_eq!(order, [20, 21, 11, 10]);
        assert!(
            c.agents
                .with_untracked(|agents| agents[0].status == AgentStatus::Blocked)
        );
    }

    #[test]
    fn a_held_move_claim_keeps_the_agent_rows_still() {
        let (c, repository) = recorded("herdr:work");
        stage_agents(
            &repository,
            vec![agent(10, "alpha", "ws-1", AgentStatus::Working, 1)],
        );
        let claim = c.claim_move().expect("free");
        block_on(c.reload_tabs());
        assert!(c.agents.get_untracked().is_empty(), "nothing published");
        drop(claim);
        block_on(c.reload_tabs());
        assert_eq!(c.agents.get_untracked().len(), 1, "…until the move lands");
    }

    #[test]
    fn a_late_change_with_a_lower_change_seq_never_overwrites_the_relisted_row() {
        let (c, repository) = recorded("herdr:work");
        stage_agents(
            &repository,
            vec![agent(10, "alpha", "ws-1", AgentStatus::Blocked, 5)],
        );
        block_on(c.refresh_agents());

        let late = agent(10, "alpha", "ws-1", AgentStatus::Working, 3);
        c.apply_agent_event(AgentEventEntry::Changed(Box::new(late)));
        assert_eq!(c.agents.get_untracked()[0].status, AgentStatus::Blocked);

        let reconciled = agent(10, "alpha", "ws-1", AgentStatus::Idle, 5);
        c.apply_agent_event(AgentEventEntry::Changed(Box::new(reconciled)));
        assert_eq!(
            c.agents.get_untracked()[0].status,
            AgentStatus::Idle,
            "an equal change_seq replaces: the server's later row is the authoritative one"
        );
    }

    #[test]
    fn a_busy_stream_lets_the_safety_relist_land_and_merges_it_by_change_seq() {
        let (c, repository) = recorded("herdr:work");
        stage_agents(
            &repository,
            vec![
                agent(10, "alpha", "ws-1", AgentStatus::Working, 1),
                agent(11, "alpha", "ws-1", AgentStatus::Idle, 1),
                agent(12, "alpha", "ws-1", AgentStatus::Idle, 1),
            ],
        );
        *repository
            .agents_delay
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Duration::from_millis(450));
        lock(&c.agents_listed_at).take();

        block_on(async {
            let stream = async {
                for seq in 2..=4 {
                    clean_signals::time::sleep(Duration::from_millis(100)).await;
                    let busy = agent(10, "alpha", "ws-1", AgentStatus::Blocked, seq);
                    c.apply_agent_event(AgentEventEntry::Changed(Box::new(busy)));
                }
                c.apply_agent_event(AgentEventEntry::Removed(11));
                let born = agent(13, "alpha", "ws-1", AgentStatus::Done, 5);
                c.apply_agent_event(AgentEventEntry::Changed(Box::new(born)));
            };
            tokio::join!(c.refresh_agents(), stream);
        });

        let rows: Vec<(u32, AgentStatus)> = c
            .agents
            .get_untracked()
            .iter()
            .map(|row| (row.pane_id, row.status))
            .collect();
        assert_eq!(
            rows,
            [
                (10, AgentStatus::Blocked),
                (13, AgentStatus::Done),
                (12, AgentStatus::Idle)
            ],
            "the streamed rows outrank the relist's older ones; the relist adds pane 12"
        );
        assert!(
            lock(&c.agents_listed_at).is_some(),
            "stamped when the relist landed, despite the events"
        );
    }

    #[test]
    fn a_poll_only_relist_replaces_the_rows_wholesale_even_when_pane_ids_were_reassigned() {
        let (c, repository) = recorded("herdr:work");
        stage_agents(
            &repository,
            vec![
                agent(1, "A", "ws-1", AgentStatus::Working, 40),
                agent(2, "B", "ws-1", AgentStatus::Working, 5),
            ],
        );
        block_on(c.refresh_agents());

        let restarted = vec![
            agent(1, "B", "ws-1", AgentStatus::Working, 5),
            agent(2, "A", "ws-1", AgentStatus::Working, 40),
        ];
        stage_agents(&repository, restarted);
        block_on(c.refresh_agents());

        let mut rows: Vec<(u32, String, u64)> = c
            .agents
            .get_untracked()
            .iter()
            .map(|row| (row.pane_id, row.tab_name.clone(), row.change_seq))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            [(1, "B".to_string(), 5), (2, "A".to_string(), 40)],
            "the relist's rows, A once and B present"
        );
    }

    #[test]
    fn merge_relist_compares_change_seq_only_for_panes_the_stream_changed() {
        let held = vec![
            agent(10, "alpha", "ws-1", AgentStatus::Blocked, 9),
            agent(11, "alpha", "ws-1", AgentStatus::Blocked, 9),
        ];
        let listed = vec![
            agent(10, "alpha", "ws-1", AgentStatus::Idle, 2),
            agent(11, "alpha", "ws-1", AgentStatus::Idle, 2),
        ];
        let touched = HashMap::from([(10, Touch::Changed)]);
        let merged = merge_relist(listed, &held, &touched);
        let status = |pane: u32| {
            merged
                .iter()
                .find(|row| row.pane_id == pane)
                .map(|row| row.status)
        };
        assert_eq!(
            status(10),
            Some(AgentStatus::Blocked),
            "touched: stream wins"
        );
        assert_eq!(
            status(11),
            Some(AgentStatus::Idle),
            "untouched: relist wins"
        );
    }

    #[test]
    fn a_stream_removal_holds_unless_the_relist_row_is_newer() {
        let held = vec![agent(10, "alpha", "ws-1", AgentStatus::Idle, 4)];
        let touched = HashMap::from([(10, Touch::Removed(4)), (11, Touch::Removed(4))]);
        let listed = vec![
            agent(10, "alpha", "ws-1", AgentStatus::Idle, 4),
            agent(11, "alpha", "ws-1", AgentStatus::Idle, 7),
        ];
        let merged = merge_relist(listed, &held, &touched);
        let panes: Vec<u32> = merged.iter().map(|row| row.pane_id).collect();
        assert_eq!(
            panes,
            [11],
            "pane 10 stays removed; pane 11 came back newer"
        );
    }

    #[test]
    fn a_relist_landing_under_a_held_move_claim_still_clears_the_stale_flag() {
        let (c, repository) = recorded("herdr:work");
        c.set_agents_stale(true);
        stage_agents(
            &repository,
            vec![agent(10, "alpha", "ws-1", AgentStatus::Blocked, 1)],
        );
        let claim = c.claim_move().expect("free");
        block_on(c.refresh_agents());
        assert!(!c.agents_stale.get_untracked());
        assert!(
            c.agents.get_untracked().is_empty(),
            "the publish waits for the move"
        );
        drop(claim);
    }

    #[test]
    fn the_agent_inbox_keeps_the_last_event_per_pane_and_a_snapshot_supersedes_the_queue() {
        let inbox = AgentInbox::default();
        let changed = |pane: u32, seq: u64| {
            AgentEventEntry::Changed(Box::new(agent(
                pane,
                "alpha",
                "ws-1",
                AgentStatus::Idle,
                seq,
            )))
        };
        inbox.push(changed(10, 1));
        inbox.push(changed(11, 1));
        inbox.push(changed(10, 2));
        inbox.push(AgentEventEntry::Removed(11));
        assert_eq!(
            block_on(inbox.next_batch()),
            Some(vec![changed(10, 2), AgentEventEntry::Removed(11)])
        );

        inbox.push(changed(10, 3));
        inbox.push(AgentEventEntry::Snapshot(Vec::new()));
        inbox.push(AgentEventEntry::Removed(10));
        assert_eq!(
            block_on(inbox.next_batch()),
            Some(vec![
                AgentEventEntry::Snapshot(Vec::new()),
                AgentEventEntry::Removed(10)
            ])
        );

        for pane in 0..100 {
            inbox.push(AgentEventEntry::Removed(pane));
        }
        let batch = block_on(inbox.next_batch()).expect("queued");
        assert_eq!(batch.len(), AGENT_INBOX_CAPACITY, "bounded");
        assert_eq!(
            batch[0],
            AgentEventEntry::Removed(36),
            "the oldest went first"
        );
        assert!(
            inbox.overflowed.load(Ordering::SeqCst),
            "and the next tick relists"
        );

        inbox.push(changed(1, 1));
        inbox.close();
        assert_eq!(block_on(inbox.next_batch()), Some(vec![changed(1, 1)]));
        assert_eq!(block_on(inbox.next_batch()), None, "closed and drained");
    }

    #[test]
    fn a_streamed_event_under_a_held_move_claim_lands_in_the_rows_but_publishes_later() {
        let (c, _repository) = recorded("herdr:work");
        c.apply_agent_event(AgentEventEntry::Snapshot(vec![
            agent(10, "alpha", "ws-1", AgentStatus::Working, 1),
            agent(11, "alpha", "ws-1", AgentStatus::Idle, 2),
        ]));
        assert_eq!(c.agents.get_untracked().len(), 2);

        let claim = c.claim_move().expect("free");
        let blocked = agent(11, "alpha", "ws-1", AgentStatus::Blocked, 3);
        c.apply_agent_event(AgentEventEntry::Changed(Box::new(blocked)));
        assert_eq!(
            c.agents.get_untracked()[0].pane_id,
            10,
            "the rows hold still"
        );
        drop(claim);
        c.publish_agent_rows();
        assert_eq!(
            c.agents.get_untracked()[0].pane_id,
            11,
            "the change was kept"
        );
    }

    #[test]
    fn an_agent_in_an_unbrowsed_space_is_peeked_then_committed_like_a_pane_row() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        let before = repository.calls().len();
        let target = agent(21, "beta", "ws-2", AgentStatus::Blocked, 1);

        assert!(block_on(c.commit_agent(&target)));

        let since: Vec<String> = repository.calls().into_iter().skip(before).collect();
        assert_eq!(
            since,
            [
                "list_tabs",
                "switch_space(ws-2)",
                "go_to_tab(2)",
                "tab_lines(beta)",
                "focus_pane(21,false)",
                "list_spaces",
            ],
            "a read-only peek, then commit_pane_in_space's own steps"
        );
        assert_eq!(focused_pane(&c, "beta"), Some(21));
    }

    #[test]
    fn an_agent_whose_tab_is_off_the_layout_moves_nothing() {
        let (c, repository) = recorded("herdr:work");
        let before = repository.actions().len();
        let mut target = agent(21, "beta", "ws-1", AgentStatus::Done, 1);
        target.tab_key.clear();
        assert!(!block_on(c.commit_agent(&target)));
        assert_eq!(repository.actions().len(), before);
    }
}
