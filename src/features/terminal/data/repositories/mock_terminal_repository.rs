//! [`MockTerminalRepository`] — the in-memory [`TerminalRepository`] every headless suite and
//! the mock-era screen run on.
//!
//! Seeds exactly what `docs/terminal.html` v2 ships for its selected space (`forgekit`) — four
//! tabs, each tab's title/name/path and its four `.term-line`s, transcribed rather than
//! paraphrased — plus two more spaces so the sheet's space pill row has something to switch
//! between: `zelli_mobile` (a second, smaller populated space) and `cupline` (empty, proving a
//! space with no tabs is a reachable answer, not a bug). Framework-free: a `Mutex`-guarded space
//! list plus a `Mutex`-guarded **per-space** tab store (`HashMap<space id, tabs>`); the mutex
//! (not a `RefCell`) is what makes it `Send + Sync`, which the controller needs to hold it
//! behind an `Arc`.
//!
//! **What mutates**: space selection, and pane focus — [`TerminalRepository::focus_pane`] moves
//! the `focused` flag within whichever space is currently selected, so a later `list_tabs` agrees
//! with the move a client just made. Each space's own tab set, its panes' identities and its
//! lines are fixed. [`TerminalRepository::list_tabs_in_space`] is the one read that never touches
//! selection — it answers whatever space id it is asked for, selected or not.
//!
//! **Headless-test tab seam.** A tab world that never moves cannot exercise the screen's reason
//! for re-reading one, so [`add_test_tab`]/[`clear_test_tabs`] append synthetic tabs to every
//! later `list_tabs` answer for the *selected* space only (`list_tabs_in_space`'s per-key peek is
//! untouched by it) — belongs to a serialized suite, since a suite reaches this store only as the
//! `Arc<dyn TerminalRepository>` its `di` published, and is process-global.
//!
//! **Agents.** A store never seeded answers [`default_agents_for_preview`] (what the desktop
//! preview shows, matching the sessions mock's herdr roll-up); the mock is session-agnostic, so a
//! zellij session's terminal lists those agents too. `watch_agents` opens with a snapshot of
//! that inventory and then delivers only what a test scripts through
//! [`push_agent_event_for_tests`]; the open/stop counters let a suite pin the subscription's
//! lifetime.
//!
//! Failure mapping: an unknown tab key, space id or blank session id maps to
//! [`AppFailure::Internal`] — a bug, since the screen only addresses what it just rendered.
//! [`MOCK_LATENCY`] is a short sleep before every call, so the screen's async path is the one the
//! real client will take.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agents::PREVIEW_AGENT_MARKS;
use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{
    AgentEntry, AgentEventEntry, AgentMark, PaneEntry, Space, TabEntry, TermLine,
};
use crate::features::terminal::domain::repositories::{AgentSink, TerminalRepository, WatchHandle};

/// Simulated round-trip latency on every call — see the [module docs](self).
const MOCK_LATENCY: Duration = Duration::from_millis(20);

/// The synthetic tabs [`add_test_tab`] appends to every later `list_tabs`
/// answer — see the [module docs](self)' headless-test tab seam.
static TEST_TABS: Mutex<Vec<TabEntry>> = Mutex::new(Vec::new());

/// Append a tab keyed (and labelled) `key` to every later `list_tabs` answer, for a
/// headless test that needs the tab world to move behind the screen's back — see
/// the [module docs](self). Never called by production code.
pub fn add_test_tab(key: &str) {
    let mut tabs = lock_test_tabs();
    // Past the selected space's (`forgekit`'s) four seeded tabs, so an appended
    // tab's index/id collides with nothing the seed already carries.
    let index = seed_tabs_forgekit().len() as u32 + tabs.len() as u32 + 1;
    tabs.push(test_tab(index, key));
}

/// Drop every tab [`add_test_tab`] appended — a suite's own teardown, and its
/// setup, since this store outlives any one mount of the screen.
pub fn clear_test_tabs() {
    lock_test_tabs().clear();
}

/// The appended-tab store, poisoned locks recovered like every other lock
/// here.
fn lock_test_tabs() -> std::sync::MutexGuard<'static, Vec<TabEntry>> {
    TEST_TABS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One appended tab, shaped exactly like a seeded one (see [`seed_tabs_forgekit`]) so
/// nothing downstream can tell the two apart.
fn test_tab(index: u32, key: &str) -> TabEntry {
    TabEntry {
        index,
        tab_id: u64::from(index),
        key: key.to_string(),
        label: key.to_string(),
        pane_title: format!("ed@{key}: ~"),
        pane_name: format!("ed@{key}"),
        pane_path: "/home/ed".to_string(),
        panes: seed_panes(index, &format!("ed@{key}"), "/home/ed"),
        active: false,
        has_bell: false,
    }
}

/// The agents [`seed_agents`] publishes — process-global like [`TEST_TABS`]. `None` (never
/// seeded) answers [`default_agents_for_preview`]; [`clear_test_agents`] stores an empty `Some`.
static TEST_AGENTS: Mutex<Option<Vec<AgentEntry>>> = Mutex::new(None);

/// Whether the next `list_agents` answers an error — see [`fail_next_list_agents_for_tests`].
static FAIL_NEXT_LIST_AGENTS: AtomicBool = AtomicBool::new(false);

/// Extra milliseconds the next `list_agents` sleeps — see [`delay_next_list_agents_for_tests`].
static DELAY_NEXT_LIST_AGENTS_MS: AtomicU64 = AtomicU64::new(0);

/// Make the next `list_agents` answer an error. Test-only seam; never called by production code.
pub fn fail_next_list_agents_for_tests() {
    FAIL_NEXT_LIST_AGENTS.store(true, Ordering::SeqCst);
}

/// Make the next `list_agents` land `by` later than usual, its answer snapshotted before the
/// wait. Test-only seam; never called by production code.
pub fn delay_next_list_agents_for_tests(by: Duration) {
    let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
    DELAY_NEXT_LIST_AGENTS_MS.store(millis, Ordering::SeqCst);
}

/// The four agents the desktop preview shows, [`PREVIEW_AGENT_MARKS`] given names, a pane
/// each in the seeded tab world, and a line of context — the same inventory the sessions
/// mock's herdr summary rolls up.
pub fn default_agents_for_preview() -> Vec<AgentEntry> {
    let details = [
        (
            "reviewer",
            ("forgekit", "minefit", 2, 20),
            "Waiting for approval to run the migration",
            "~/api",
            4,
        ),
        (
            "planner",
            ("forgekit", "gohora", 3, 30),
            "Finished the refactor plan",
            "~/src",
            3,
        ),
        (
            "scout",
            ("forgekit", "devbox", 4, 40),
            "Reading the build logs",
            "/home/ed",
            2,
        ),
        (
            "notes",
            ("zelli_mobile", "logs", 101, 10),
            "Idle since the last summary",
            "~/logs",
            1,
        ),
    ];
    PREVIEW_AGENT_MARKS
        .iter()
        .zip(details)
        .map(
            |(
                &(status, kind),
                (name, (space, tab_key, tab_id, pane_id), summary, cwd, change_seq),
            )| {
                AgentEntry {
                    pane_id,
                    tab_id,
                    tab_key: tab_key.to_string(),
                    space_id: Some(space.to_string()),
                    space_name: Some(space.to_string()),
                    tab_name: tab_key.to_string(),
                    kind: kind.to_string(),
                    name: Some(name.to_string()),
                    display_kind: None,
                    status,
                    title: Some(format!("{kind} \u{b7} {name}")),
                    summary: Some(summary.to_string()),
                    cwd: Some(cwd.to_string()),
                    focused: false,
                    change_seq,
                }
            },
        )
        .collect()
}

/// Replace the agent inventory every later `list_agents` answers, and mark the pane each one
/// targets (matched by pane id and tab id) in every later tab/pane read. Never called by
/// production code.
pub fn seed_agents(agents: Vec<AgentEntry>) {
    *lock_test_agents() = Some(agents);
}

/// Drop every seeded agent — a suite's own teardown and setup.
pub fn clear_test_agents() {
    *lock_test_agents() = Some(Vec::new());
}

fn lock_test_agents() -> std::sync::MutexGuard<'static, Option<Vec<AgentEntry>>> {
    TEST_AGENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The inventory `list_agents` and [`mark_agents`] both read: the seeded one, else the default.
fn resolved_agents() -> Vec<AgentEntry> {
    lock_test_agents()
        .clone()
        .unwrap_or_else(default_agents_for_preview)
}

/// The one open agents subscription: its sink, its liveness, and the id its handle stops by.
struct MockWatch {
    id: u64,
    sink: AgentSink,
    live: Arc<AtomicBool>,
}

/// The newest `watch_agents` subscription — process-global like [`TEST_AGENTS`]; a newer
/// one replaces it, dropping the older sink.
static AGENT_WATCH: Mutex<Option<MockWatch>> = Mutex::new(None);

/// Mints [`MockWatch::id`]s, so a stale handle's stop never clears a newer subscription.
static NEXT_WATCH_ID: AtomicU64 = AtomicU64::new(1);

/// How many subscriptions `watch_agents` has opened.
static AGENT_WATCH_OPENS: AtomicUsize = AtomicUsize::new(0);

/// How many [`WatchHandle`]s the mock has seen stopped.
static AGENT_WATCH_STOPS: AtomicUsize = AtomicUsize::new(0);

fn lock_agent_watch() -> std::sync::MutexGuard<'static, Option<MockWatch>> {
    AGENT_WATCH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Deliver `event` on the open subscription, which counts as its stream being live again;
/// `false` when none is open. Never called by production code.
pub fn push_agent_event_for_tests(event: AgentEventEntry) -> bool {
    let watch = lock_agent_watch();
    let Some(watch) = watch.as_ref() else {
        return false;
    };
    watch.live.store(true, Ordering::Release);
    (watch.sink)(event);
    true
}

/// Mark the open subscription's stream lost, as a reconnect waiting out its backoff reports it.
/// Never called by production code.
pub fn drop_agent_stream_for_tests() {
    if let Some(watch) = lock_agent_watch().as_ref() {
        watch.live.store(false, Ordering::Release);
    }
}

/// Whether a subscription is open (opened, and its handle not yet dropped).
pub fn agent_watch_open_for_tests() -> bool {
    lock_agent_watch().is_some()
}

/// How many subscriptions `watch_agents` has opened, process-wide.
pub fn agent_watch_opens_for_tests() -> usize {
    AGENT_WATCH_OPENS.load(Ordering::SeqCst)
}

/// How many subscription handles have been dropped, process-wide.
pub fn agent_watch_stops_for_tests() -> usize {
    AGENT_WATCH_STOPS.load(Ordering::SeqCst)
}

/// Stamp `PaneEntry.agent` on each pane a seeded agent targets.
fn mark_agents(mut tabs: Vec<TabEntry>) -> Vec<TabEntry> {
    let agents = resolved_agents();
    if agents.is_empty() {
        return tabs;
    }
    for tab in &mut tabs {
        for pane in &mut tab.panes {
            pane.agent = agents
                .iter()
                .find(|a| a.pane_id == pane.id && a.tab_id == tab.tab_id)
                .map(|a| AgentMark {
                    kind: a.kind.clone(),
                    name: a.name.clone(),
                    status: a.status,
                });
        }
    }
    tabs
}

/// The most verbs [`VERB_LOG`] keeps.
const VERB_LOG_CAP: usize = 64;

/// Every landed navigation verb (`switch_space:<id>`, `go_to_tab:<id>`,
/// `focus_pane:<id>`), in call order — process-global like [`TEST_TABS`], so
/// only a serialized suite reads it. Capped at [`VERB_LOG_CAP`]: the oldest verb
/// drops first, so a long preview session cannot grow it.
static VERB_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The navigation verbs this store has answered `Ok` to since the last
/// [`clear_verb_log_for_tests`]. Never read by production code.
pub fn verb_log_for_tests() -> Vec<String> {
    lock_verb_log().clone()
}

/// Forget every recorded verb — a suite's own setup.
pub fn clear_verb_log_for_tests() {
    lock_verb_log().clear();
}

fn lock_verb_log() -> std::sync::MutexGuard<'static, Vec<String>> {
    VERB_LOG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn log_verb(verb: &str, target: impl std::fmt::Display) {
    let mut log = lock_verb_log();
    if log.len() >= VERB_LOG_CAP {
        log.remove(0);
    }
    log.push(format!("{verb}:{target}"));
}

/// An in-memory [`TerminalRepository`]. See the [module docs](self).
pub struct MockTerminalRepository {
    /// The space list — one of the two things this store lets move.
    spaces: Mutex<Vec<Space>>,
    /// The tab world, keyed by [`Space::id`]: a fixed per-space set of tabs whose pane
    /// **focus** moves within the currently *selected* space (see the [module docs](self)'
    /// "What mutates").
    tabs_by_space: Mutex<HashMap<String, Vec<TabEntry>>>,
}

impl MockTerminalRepository {
    /// A repository seeded with [`seed_spaces`]/[`seed_tabs_by_space`].
    pub fn seeded() -> Self {
        Self {
            spaces: Mutex::new(seed_spaces()),
            tabs_by_space: Mutex::new(seed_tabs_by_space()),
        }
    }

    /// Select the space with id `id`, deselecting every other — the body
    /// [`TerminalRepository::switch_space`] runs, exposed synchronously so a test
    /// can move the selection without a runtime.
    pub fn select_space(&self, id: &str) {
        for space in self.lock().iter_mut() {
            space.selected = space.id == id;
        }
    }

    /// The space list, cloned — a synchronous peek for tests, never part of
    /// the trait contract production code speaks.
    pub fn spaces_snapshot(&self) -> Vec<Space> {
        self.lock().clone()
    }

    /// The id of the currently selected space, or `None` if somehow nothing is (never
    /// true of [`seeded`](Self::seeded) — exactly one space is always selected).
    fn selected_space_id(&self) -> Option<String> {
        self.lock()
            .iter()
            .find(|space| space.selected)
            .map(|space| space.id.clone())
    }

    /// The *selected* space's seeded tabs plus whatever [`add_test_tab`] appended, cloned. An
    /// id [`seed_tabs_by_space`] carries no entry for (unreachable through [`seeded`](Self::seeded),
    /// defensive otherwise) answers empty rather than panicking.
    fn tabs_snapshot(&self) -> Vec<TabEntry> {
        let mut tabs = self
            .selected_space_id()
            .and_then(|id| self.lock_tabs().get(&id).cloned())
            .unwrap_or_default();
        tabs.extend(lock_test_tabs().iter().cloned());
        mark_agents(tabs)
    }

    /// The store, with a poisoned lock recovered rather than propagated: a
    /// panicking test must not cascade into every later call.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Space>> {
        self.spaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The per-space tab store, recovered the same way [`lock`](Self::lock) is.
    fn lock_tabs(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<TabEntry>>> {
        self.tabs_by_space
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for MockTerminalRepository {
    fn default() -> Self {
        Self::seeded()
    }
}

/// The seeded space list — `docs/terminal.html`'s `spaces` pill row, in order,
/// with `forgekit` selected (`.pill.sel`).
fn seed_spaces() -> Vec<Space> {
    [
        ("cupline", false),
        ("zelli_mobile", false),
        ("forgekit", true),
    ]
    .into_iter()
    .map(|(name, selected)| Space {
        id: name.to_string(),
        name: name.to_string(),
        selected,
    })
    .collect()
}

/// `forgekit`'s seeded tab world — `docs/terminal.html`'s `TABS` map, in the order its
/// `#sheetTabRow` pills draw, with `devbox` active (`.pill.sel`). Each tab also
/// carries the **pane list** the sheet lists and commits against: the mock's own
/// `.pane-item` plus one unfocused sibling, so the browse-then-commit flow has more
/// than one row to move a focus between. Pane ids are `<tab index>0`/`<tab index>1`
/// — this store fronts no backend, so an id only has to be stable and unique *within
/// its own space* (see [`seed_tabs_by_space`] — `focus_pane` only ever searches the
/// currently selected one, so nothing requires ids to be unique *across* spaces too).
fn seed_tabs_forgekit() -> Vec<TabEntry> {
    let tab = |index: u32, key: &str, label: &str, title: &str, name: &str, path: &str| TabEntry {
        index,
        tab_id: u64::from(index),
        key: key.to_string(),
        label: label.to_string(),
        pane_title: title.to_string(),
        pane_name: name.to_string(),
        pane_path: path.to_string(),
        panes: seed_panes(index, name, path),
        active: key == "devbox",
        has_bell: false,
    };
    let mut tabs = vec![
        tab(
            1,
            "btop",
            "btop",
            "ed@precision: ~",
            "ed@precision",
            "/home/ed",
        ),
        tab(
            2,
            "minefit",
            "minefit-api",
            "ed@minefit: ~/api",
            "ed@minefit",
            "~/api",
        ),
        tab(
            3,
            "gohora",
            "gohora",
            "ed@gohora: ~/src",
            "ed@gohora",
            "~/src",
        ),
        tab(
            4,
            "devbox",
            "devbox",
            "ed@devbox: ~",
            "ed@devbox",
            "/home/ed",
        ),
    ];
    // One of each mark the docked rail draws: a ringing tab, an exited pane,
    // and a floating one under the tab the rail rests on.
    tabs[2].has_bell = true;
    tabs[0].panes[1].exited = true;
    tabs[3].panes[1].floating = true;
    tabs
}

/// `zelli_mobile`'s seeded tab world — a second, smaller populated space so
/// [`TerminalRepository::list_tabs_in_space`](super::super::domain::repositories::TerminalRepository::list_tabs_in_space)
/// has a non-selected space to answer distinctly from `forgekit`'s. Two tabs, `logs` active.
fn seed_tabs_zelli_mobile() -> Vec<TabEntry> {
    let tab = |index: u32, key: &str, path: &str, active: bool| TabEntry {
        index,
        tab_id: u64::from(100 + index),
        key: key.to_string(),
        label: key.to_string(),
        pane_title: format!("ed@zelli: {path}"),
        pane_name: "ed@zelli".to_string(),
        pane_path: path.to_string(),
        panes: seed_panes(index, "ed@zelli", path),
        active,
        has_bell: false,
    };
    vec![
        tab(1, "logs", "~/logs", true),
        tab(2, "worker", "~/worker", false),
    ]
}

/// Every seeded space's tab world, keyed by [`Space::id`] — `forgekit` (multi-tab, the
/// default selection), `zelli_mobile` (a second, smaller populated space), and `cupline`
/// (empty — a space with no tabs is a reachable answer, not a bug).
fn seed_tabs_by_space() -> HashMap<String, Vec<TabEntry>> {
    HashMap::from([
        ("cupline".to_string(), Vec::new()),
        ("zelli_mobile".to_string(), seed_tabs_zelli_mobile()),
        ("forgekit".to_string(), seed_tabs_forgekit()),
    ])
}

/// One tab's two panes: the mock's own focused `.pane-item`, and an unfocused `zsh` beside it
/// — see [`seed_tabs_forgekit`]/[`seed_tabs_zelli_mobile`].
///
/// Side by side, splitting a classic 80x24 grid down the middle, rather than sharing one
/// full-grid rect: `presentation::pages`' content-tap hit test resolves a tapped cell against
/// these rects, and identical ones would resolve every in-bounds tap to the same pane, leaving
/// no cell to aim at the *other* one. Two disjoint halves make both panes tappable, which is
/// what a tap's focus-or-keyboard verdict turns on.
fn seed_panes(index: u32, name: &str, path: &str) -> Vec<PaneEntry> {
    let pane = |id: u32, title: String, focused: bool, x: u32| PaneEntry {
        agent: None,
        id,
        title,
        cwd: path.to_string(),
        focused,
        floating: false,
        exited: false,
        is_plugin: false,
        is_fullscreen: false,
        x,
        y: 0,
        rows: 24,
        cols: 40,
    };
    vec![
        pane(index * 10, name.to_string(), true, 0),
        pane(index * 10 + 1, format!("{name} zsh"), false, 40),
    ]
}

/// One tab's lines — `TABS[key].lines`, transcribed span for span (`t:'p'` →
/// [`TermLine::prompt`], `'out'` → output, `'cm'` → comment, and the trailing
/// `cursor:true` prompt → [`TermLine::caret`]).
fn seed_lines(key: &str) -> Option<Vec<TermLine>> {
    let lines = match key {
        "btop" => vec![
            TermLine::prompt("ed@precision:~$ btop"),
            TermLine::output("cpu 12% \u{b7} mem 4.1/16gb \u{b7} net 1.2mb/s"),
            TermLine::comment("# live system monitor, refreshes every 2s"),
            TermLine::caret("ed@precision:~$ "),
        ],
        "minefit" => vec![
            TermLine::prompt("ed@minefit:~/api$ cargo run --release"),
            TermLine::output("Compiling minefit-api v0.3.2"),
            TermLine::output("Finished release [optimized] in 12.4s"),
            TermLine::caret("ed@minefit:~/api$ "),
        ],
        "gohora" => vec![
            TermLine::prompt("ed@gohora:~/src$ go build ./..."),
            TermLine::output("ok  gohora/cmd/server  0.842s"),
            TermLine::comment("# no errors, 3 packages rebuilt"),
            TermLine::caret("ed@gohora:~/src$ "),
        ],
        "devbox" => vec![
            TermLine::prompt("ed@devbox:~$ systemctl status muxrd"),
            TermLine::output("Active: active (running) since Jul 04 10:31:23"),
            TermLine::comment("# staggered per-line reveal \u{2014} replays on tab switch"),
            TermLine::caret("ed@devbox:~$ "),
        ],
        _ => return None,
    };
    Some(lines)
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl TerminalRepository for MockTerminalRepository {
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        Ok(self.spaces_snapshot())
    }

    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        Ok(self.tabs_snapshot())
    }

    async fn list_tabs_in_space(&self, space_id: &str) -> Result<Vec<TabEntry>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        // A read-only peek: answers whatever `space_id` names — selected or not — and
        // never touches `Space.selected` or appends the headless-test seam's tabs (those
        // ride only on the *selected* space's own `list_tabs` answer).
        if !self.lock().iter().any(|space| space.id == space_id) {
            return Err(AppFailure::Internal(format!(
                "list_tabs_in_space: no space {space_id}"
            )));
        }
        Ok(mark_agents(
            self.lock_tabs().get(space_id).cloned().unwrap_or_default(),
        ))
    }

    async fn list_agents(&self) -> Result<Vec<AgentEntry>, AppFailure> {
        let answer = resolved_agents();
        let delay = DELAY_NEXT_LIST_AGENTS_MS.swap(0, Ordering::SeqCst);
        clean_signals::time::sleep(MOCK_LATENCY + Duration::from_millis(delay)).await;
        if FAIL_NEXT_LIST_AGENTS.swap(false, Ordering::SeqCst) {
            return Err(AppFailure::Internal("list_agents: seeded failure".into()));
        }
        Ok(answer)
    }

    fn watch_agents(&self, sink: AgentSink) -> Result<WatchHandle, AppFailure> {
        let id = NEXT_WATCH_ID.fetch_add(1, Ordering::SeqCst);
        let live = Arc::new(AtomicBool::new(true));
        // The wire's first message is always the whole inventory.
        sink(AgentEventEntry::Snapshot(resolved_agents()));
        *lock_agent_watch() = Some(MockWatch {
            id,
            sink,
            live: Arc::clone(&live),
        });
        AGENT_WATCH_OPENS.fetch_add(1, Ordering::SeqCst);
        Ok(WatchHandle::new(live, move || {
            AGENT_WATCH_STOPS.fetch_add(1, Ordering::SeqCst);
            let mut watch = lock_agent_watch();
            if watch.as_ref().is_some_and(|watch| watch.id == id) {
                *watch = None;
            }
        }))
    }

    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        seed_lines(&key).ok_or_else(|| AppFailure::Internal(format!("tab_lines: no tab {key}")))
    }

    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        if !self.lock().iter().any(|space| space.id == space_id) {
            return Err(AppFailure::Internal(format!(
                "switch_space: no space {space_id}"
            )));
        }
        self.select_space(&space_id);
        log_verb("switch_space", &space_id);
        Ok(())
    }

    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        // Nothing to tell: this store fronts no session, and the card's own switch
        // is `tab_lines`. An id no seeded tab carries is still a bug worth
        // surfacing — the sheet only addresses a tab it just drew.
        if !self.tabs_snapshot().iter().any(|tab| tab.tab_id == tab_id) {
            return Err(AppFailure::Internal(format!("go_to_tab: no tab {tab_id}")));
        }
        log_verb("go_to_tab", tab_id);
        Ok(())
    }

    async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        if id.trim().is_empty() {
            return Err(AppFailure::Internal(
                "detach_session called with a blank id".to_string(),
            ));
        }
        // Nothing to drop: there is no real attachment until the `muxrd`
        // client lands (see the module docs).
        Ok(())
    }

    async fn focus_pane(&self, pane_id: u32, is_plugin: bool) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        // A pane no seeded tab carries is a bug worth surfacing, since the
        // sheet only ever commits a row it just drew. Scoped to the *selected*
        // space, like every other write this store performs.
        let addressed = |pane: &PaneEntry| pane.id == pane_id && pane.is_plugin == is_plugin;
        let Some(space_id) = self.selected_space_id() else {
            return Err(AppFailure::Internal(format!(
                "focus_pane: no pane {pane_id}"
            )));
        };
        let mut tabs_by_space = self.lock_tabs();
        let Some(tabs) = tabs_by_space.get_mut(&space_id) else {
            return Err(AppFailure::Internal(format!(
                "focus_pane: no pane {pane_id}"
            )));
        };
        let Some(tab) = tabs.iter_mut().find(|tab| tab.panes.iter().any(addressed)) else {
            return Err(AppFailure::Internal(format!(
                "focus_pane: no pane {pane_id}"
            )));
        };
        // One focused pane per tab, and it is the one just addressed — so a client
        // re-reading the layout is told what it just asked for.
        for pane in &mut tab.panes {
            let focused = addressed(pane);
            pane.focused = focused;
        }
        log_verb("focus_pane", pane_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::terminal::domain::entities::{AgentStatus, TermLineKind};

    /// Drive one repository call to completion on a current-thread runtime —
    /// a pure data-layer test, with no frust runtime in play.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn seeded_agents_round_trip_and_mark_their_pane() {
        let repo = MockTerminalRepository::seeded();
        clear_test_agents();
        assert!(block_on(repo.list_agents()).expect("ok").is_empty());

        let tab = block_on(repo.list_tabs()).expect("tabs").remove(0);
        let agent = AgentEntry {
            pane_id: tab.panes[0].id,
            tab_id: tab.tab_id,
            tab_key: tab.key.clone(),
            space_id: None,
            space_name: None,
            tab_name: tab.label.clone(),
            kind: "claude".to_string(),
            name: None,
            display_kind: None,
            status: AgentStatus::Working,
            title: None,
            summary: None,
            cwd: None,
            focused: true,
            change_seq: 1,
        };
        seed_agents(vec![agent.clone()]);
        assert_eq!(block_on(repo.list_agents()).expect("ok"), vec![agent]);
        let marked = block_on(repo.list_tabs()).expect("tabs").remove(0);
        let mark = marked.panes[0].agent.as_ref().expect("marked");
        assert_eq!(
            (mark.kind.as_str(), mark.status),
            ("claude", AgentStatus::Working)
        );
        assert!(marked.panes[1].agent.is_none());

        clear_test_agents();
        assert!(block_on(repo.list_agents()).expect("ok").is_empty());
    }

    #[test]
    fn a_watch_opens_with_a_snapshot_delivers_pushes_and_stops_on_drop() {
        let repo = MockTerminalRepository::seeded();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let stops = agent_watch_stops_for_tests();
        let handle = repo
            .watch_agents(Box::new(move |event| {
                sink_seen.lock().expect("unpoisoned").push(event);
            }))
            .expect("the mock watches");
        assert!(agent_watch_open_for_tests() && handle.is_live());
        drop_agent_stream_for_tests();
        assert!(!handle.is_live(), "a lost stream reads not live");
        assert!(push_agent_event_for_tests(AgentEventEntry::Removed(7)));
        assert!(handle.is_live(), "a delivered event is a live stream again");
        let seen = seen.lock().expect("unpoisoned").clone();
        assert!(matches!(seen[0], AgentEventEntry::Snapshot(_)), "{seen:?}");
        assert_eq!(seen[1..], [AgentEventEntry::Removed(7)]);
        drop(handle);
        assert_eq!(agent_watch_stops_for_tests(), stops + 1);
        assert!(!agent_watch_open_for_tests());
        assert!(!push_agent_event_for_tests(AgentEventEntry::Removed(7)));
    }

    #[test]
    fn every_default_preview_agent_points_at_a_real_pane_and_its_tab_key() {
        let repo = MockTerminalRepository::seeded();
        let agents = default_agents_for_preview();
        let mut panes = std::collections::HashSet::new();
        for agent in &agents {
            let space = agent.space_id.clone().expect("a space");
            let tabs = block_on(repo.list_tabs_in_space(&space)).expect("tabs");
            let tab = tabs
                .iter()
                .find(|tab| tab.tab_id == agent.tab_id)
                .unwrap_or_else(|| panic!("{}: no tab {}", agent.display_name(), agent.tab_id));
            assert_eq!(tab.key, agent.tab_key, "{}", agent.display_name());
            assert!(
                tab.panes.iter().any(|pane| pane.id == agent.pane_id),
                "{}: no pane {}",
                agent.display_name(),
                agent.pane_id
            );
            assert!(
                panes.insert((space, agent.tab_id, agent.pane_id)),
                "one agent per pane"
            );
        }
        assert_eq!(agents.len(), 4);
    }

    #[test]
    fn the_seeded_spaces_are_the_mocks_pill_row() {
        let repo = MockTerminalRepository::seeded();
        let spaces = block_on(repo.list_spaces()).expect("the mock never fails to list");

        let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["cupline", "zelli_mobile", "forgekit"]);
        let selected: Vec<&str> = spaces
            .iter()
            .filter(|s| s.selected)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(selected, ["forgekit"], "exactly one `.pill.sel`");
    }

    #[test]
    fn the_seeded_tabs_are_the_mocks_four_with_devbox_active() {
        let repo = MockTerminalRepository::seeded();
        let tabs = block_on(repo.list_tabs()).expect("the mock never fails to list");

        let pills: Vec<String> = tabs.iter().map(TabEntry::pill_label).collect();
        assert_eq!(pills, ["1 btop", "2 minefit-api", "3 gohora", "4 devbox"]);
        let active: Vec<&str> = tabs
            .iter()
            .filter(|t| t.active)
            .map(|t| t.key.as_str())
            .collect();
        assert_eq!(active, ["devbox"]);

        // The pane facts the card's status row and the sheet's pane item print.
        let devbox = tabs.last().expect("four tabs");
        assert_eq!(devbox.pane_title, "ed@devbox: ~");
        assert_eq!(devbox.pane_name, "ed@devbox");
        assert_eq!(devbox.pane_path, "/home/ed");
    }

    #[test]
    fn the_active_tabs_lines_are_the_mocks_verbatim() {
        let repo = MockTerminalRepository::seeded();
        let lines = block_on(repo.tab_lines("devbox".to_string())).expect("devbox exists");

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].kind, TermLineKind::Prompt);
        assert_eq!(lines[0].text, "ed@devbox:~$ systemctl status muxrd");
        assert_eq!(lines[1].kind, TermLineKind::Output);
        assert_eq!(
            lines[1].text,
            "Active: active (running) since Jul 04 10:31:23"
        );
        assert_eq!(lines[2].kind, TermLineKind::Comment);
        assert_eq!(
            lines[2].text,
            "# staggered per-line reveal \u{2014} replays on tab switch"
        );
        assert_eq!(lines[3].text, "ed@devbox:~$ ");
        assert!(
            lines[3].cursor,
            "only the trailing prompt carries the caret"
        );
        assert_eq!(
            lines.iter().filter(|l| l.cursor).count(),
            1,
            "one caret per tab"
        );
    }

    #[test]
    fn every_tab_has_lines_and_an_unknown_key_fails() {
        let repo = MockTerminalRepository::seeded();
        for key in ["btop", "minefit", "gohora", "devbox"] {
            let lines = block_on(repo.tab_lines(key.to_string()))
                .unwrap_or_else(|_| panic!("{key} has lines"));
            assert_eq!(lines.len(), 4, "{key}");
            assert!(
                lines.last().is_some_and(|l| l.cursor),
                "{key} ends in a caret"
            );
        }
        assert!(block_on(repo.tab_lines("nope".to_string())).is_err());
    }

    #[test]
    fn selecting_a_space_moves_the_selection_rather_than_adding_one() {
        let repo = MockTerminalRepository::seeded();
        repo.select_space("cupline");

        let selected: Vec<String> = repo
            .spaces_snapshot()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.name)
            .collect();
        assert_eq!(selected, ["cupline"]);
    }

    #[test]
    fn a_space_switch_writes_the_store_and_an_unknown_id_fails() {
        let repo = MockTerminalRepository::seeded();
        assert!(block_on(repo.switch_space("cupline".to_string())).is_ok());

        let selected: Vec<String> = repo
            .spaces_snapshot()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.id)
            .collect();
        assert_eq!(selected, ["cupline"], "the switch is the one write path");
        assert!(block_on(repo.switch_space("nope".to_string())).is_err());
    }

    #[test]
    fn a_tab_switch_succeeds_for_every_seeded_id_and_fails_for_no_other() {
        let repo = MockTerminalRepository::seeded();
        for tab_id in 1..=4 {
            assert!(block_on(repo.go_to_tab(tab_id)).is_ok(), "{tab_id}");
        }
        assert!(block_on(repo.go_to_tab(99)).is_err());
    }

    #[test]
    fn every_tab_carries_two_panes_with_exactly_one_focused() {
        let repo = MockTerminalRepository::seeded();
        let tabs = block_on(repo.list_tabs()).expect("the mock never fails to list");

        for tab in &tabs {
            assert_eq!(tab.panes.len(), 2, "{}", tab.key);
            let focused: Vec<u32> = tab
                .panes
                .iter()
                .filter(|pane| pane.focused)
                .map(|pane| pane.id)
                .collect();
            assert_eq!(focused, [tab.index * 10], "{}", tab.key);
            // The mock's own `.pane-item` is the focused one, verbatim.
            assert_eq!(tab.panes[0].title, tab.pane_name);
            assert_eq!(tab.panes[0].cwd, tab.pane_path);
        }
        // …and no two panes share an id across the whole seed.
        let mut ids: Vec<u32> = tabs
            .iter()
            .flat_map(|t| t.panes.iter().map(|p| p.id))
            .collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "pane ids are unique");
    }

    #[test]
    fn a_pane_focus_succeeds_for_every_seeded_pane_and_fails_for_no_other() {
        let repo = MockTerminalRepository::seeded();
        for tab in block_on(repo.list_tabs()).expect("the mock never fails to list") {
            for pane in tab.panes {
                assert!(
                    block_on(repo.focus_pane(pane.id, pane.is_plugin)).is_ok(),
                    "{}",
                    pane.id
                );
            }
        }
        assert!(block_on(repo.focus_pane(999, false)).is_err());
        // The two id namespaces are separate — a plugin pane with a terminal
        // pane's id is still not a pane this store carries.
        assert!(block_on(repo.focus_pane(10, true)).is_err());
    }

    #[test]
    fn a_pane_focus_moves_the_focused_flag_so_a_later_list_agrees_with_it() {
        let repo = MockTerminalRepository::seeded();
        // The second pane of the first tab — the mock seeds the first focused.
        assert!(block_on(repo.focus_pane(11, false)).is_ok());

        let tabs = block_on(repo.list_tabs()).expect("the mock never fails to list");
        let focused: Vec<u32> = tabs[0]
            .panes
            .iter()
            .filter(|pane| pane.focused)
            .map(|pane| pane.id)
            .collect();
        assert_eq!(
            focused,
            [11],
            "one focused pane per tab, and it is the one just addressed"
        );
        assert_eq!(
            tabs[1]
                .panes
                .iter()
                .filter(|pane| pane.focused)
                .map(|pane| pane.id)
                .collect::<Vec<u32>>(),
            [20],
            "…and no other tab's focus moved with it"
        );
    }

    #[test]
    fn the_seed_carries_one_bell_one_exited_pane_and_one_floating_pane() {
        let tabs = seed_tabs_forgekit();
        let bells: Vec<&str> = tabs
            .iter()
            .filter(|tab| tab.has_bell)
            .map(|tab| tab.key.as_str())
            .collect();
        assert_eq!(bells, ["gohora"]);
        let panes = || tabs.iter().flat_map(|tab| tab.panes.iter());
        assert_eq!(panes().filter(|pane| pane.exited).count(), 1);
        let floating: Vec<u32> = panes()
            .filter(|pane| pane.floating)
            .map(|pane| pane.id)
            .collect();
        assert_eq!(floating, [41], "under devbox, the tab the rail rests on");
    }

    #[test]
    fn an_appended_test_tab_is_shaped_exactly_like_a_seeded_one() {
        // The seam's *effect* on `list_tabs` is exercised from
        // `tests/terminal_live.rs`, whose suite is serialized — this binary runs its
        // tests in parallel, and the appended-tab store is process-global.
        let appended = test_tab(5, "scratch");
        assert_eq!(appended.pill_label(), "5 scratch");
        assert_eq!(appended.tab_id, 5);
        assert!(
            !seed_tabs_forgekit()
                .iter()
                .any(|tab| tab.tab_id == appended.tab_id),
            "an appended tab's id collides with no seeded one"
        );
        assert_eq!(
            appended.panes.len(),
            seed_tabs_forgekit()[0].panes.len(),
            "…and it carries the same pane shape"
        );
    }

    #[test]
    fn detach_rejects_a_blank_id() {
        let repo = MockTerminalRepository::seeded();
        assert!(block_on(repo.detach_session("s-1".to_string())).is_ok());
        assert!(block_on(repo.detach_session("  ".to_string())).is_err());
    }

    // -- space-scoped tab listing ---------------------------------------------

    #[test]
    fn list_tabs_in_space_answers_a_non_selected_spaces_own_snapshot() {
        let repo = MockTerminalRepository::seeded();
        let tabs = block_on(repo.list_tabs_in_space("zelli_mobile"))
            .expect("zelli_mobile is a seeded space");

        let keys: Vec<&str> = tabs.iter().map(|tab| tab.key.as_str()).collect();
        assert_eq!(keys, ["logs", "worker"]);
    }

    #[test]
    fn list_tabs_in_space_never_moves_selection_and_list_tabs_still_answers_the_selected_space() {
        let repo = MockTerminalRepository::seeded();
        assert!(
            block_on(repo.list_tabs_in_space("zelli_mobile")).is_ok(),
            "a read-only peek at another space"
        );

        let selected: Vec<String> = repo
            .spaces_snapshot()
            .into_iter()
            .filter(|space| space.selected)
            .map(|space| space.id)
            .collect();
        assert_eq!(
            selected,
            ["forgekit"],
            "the peek left the seeded selection untouched"
        );

        let pills: Vec<String> = block_on(repo.list_tabs())
            .expect("the mock never fails to list")
            .iter()
            .map(TabEntry::pill_label)
            .collect();
        assert_eq!(
            pills,
            ["1 btop", "2 minefit-api", "3 gohora", "4 devbox"],
            "list_tabs still answers the selected space (forgekit), unaffected by the peek"
        );
    }

    #[test]
    fn list_tabs_in_space_answers_an_empty_space_with_an_empty_snapshot() {
        let repo = MockTerminalRepository::seeded();
        assert_eq!(
            block_on(repo.list_tabs_in_space("cupline")),
            Ok(Vec::new()),
            "a space with no tabs is a reachable answer, not a bug"
        );
    }

    #[test]
    fn list_tabs_in_space_fails_for_an_unknown_space_id() {
        let repo = MockTerminalRepository::seeded();
        assert!(matches!(
            block_on(repo.list_tabs_in_space("nope")),
            Err(AppFailure::Internal(_))
        ));
    }

    #[test]
    fn switch_space_still_moves_selection_the_way_list_tabs_in_space_never_does() {
        let repo = MockTerminalRepository::seeded();
        assert!(block_on(repo.switch_space("zelli_mobile".to_string())).is_ok());

        let selected: Vec<String> = repo
            .spaces_snapshot()
            .into_iter()
            .filter(|space| space.selected)
            .map(|space| space.id)
            .collect();
        assert_eq!(selected, ["zelli_mobile"]);

        let pills: Vec<String> = block_on(repo.list_tabs())
            .expect("the mock never fails to list")
            .iter()
            .map(TabEntry::pill_label)
            .collect();
        assert_eq!(
            pills,
            ["1 logs", "2 worker"],
            "list_tabs now answers the newly-selected space"
        );
    }
}
