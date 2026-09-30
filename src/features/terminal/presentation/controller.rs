//! [`TerminalController`] — the `/terminal` screen's view model: a
//! [`ControllerCore`], five use cases over two repositories, and `settings`'
//! preferences store, resolved once by `pages.rs`'s `use_controller` factory.
//! Per-frame reveal progress lives off-signal in `super::chrome`; this
//! publishes only the replay cue ([`reveal_epoch`](TerminalController::reveal_epoch)).
//!
//! **Feed vs repository**: the feed ([`TerminalFeed`]) is a byte stream,
//! separate from [`TerminalRepository`]'s one-shot calls, and writes no
//! signal itself — [`mirror_link`](TerminalController::mirror_link), polled
//! from the UI-thread repaint pump, is the one place link status becomes one.
//!
//! **No server push for tabs/panes**, so every refresh races freely; one
//! reconciliation covers them all: a generation counter that commits only the
//! newest fetch, read-compare-then-write in
//! [`publish_tabs`](TerminalController::publish_tabs), a self-healing active
//! key, and single-flight claims on the poll, on creates, and on every **move**
//! — the sheet's commit and the space switches share one claim, since either
//! can move the tab world out from under the other.
//!
//! **The tree sheet's own browse state** is id-keyed
//! ([`expanded_space`](TerminalController::expanded_space)/
//! [`expanded_tab`](TerminalController::expanded_tab), at most one of each): expanding a
//! non-selected space is a read-only [`ListSpaceTabs`] peek
//! ([`expand_space`](TerminalController::expand_space)), cached under
//! [`browsed_space_tabs`](TerminalController::browsed_space_tabs) rather than
//! duplicated onto [`tabs`](TerminalController::tabs), and committing a pane under one
//! ([`commit_pane_in_space`](TerminalController::commit_pane_in_space)) switches to it
//! first — moving [`spaces`](TerminalController::spaces)' own selection with it — unlike
//! the peek itself.
//!
//! [`is_herdr`] is the one backend discriminator: only herdr has spaces and a
//! resume hint ([`ResumeSlot`]); the grid's content-tap hook is never wired there.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clean_signals::{ControllerCore, RunOptions};
use clean_signals_frust::use_interval;
use frust::authoring::text::FontFamily;
use frust::{GetUntracked, RwSignal, Set, WithUntracked, spawn_local};

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::terminal::data::feeds::demo_feed::DemoFeed;
use crate::features::terminal::data::feeds::grpc_feed::{FeedStatus, GrpcTerminalFeed, ResumeSlot};
use crate::features::terminal::domain::entities::{
    PaneEntry, RenameRequest, ScrollStep, SettingsCursorStyle, Space, TabEntry, TermLine, Workspace,
};
use crate::features::terminal::domain::feed::TerminalFeed;
use crate::features::terminal::domain::repositories::{
    PreferencesRepository, SessionsRepository, TerminalRepository,
};
use crate::features::terminal::domain::use_cases::{
    DetachSession, ListSpaceTabs, LoadWorkspace, RenameAttachedSession, SwitchTab, WorkspaceQuery,
};
use crate::fonts;

use super::term;

/// The `/terminal` screen's view model. See the [module docs](self).
pub struct TerminalController {
    core: ControllerCore<AppFailure>,
    load: LoadWorkspace,
    switch: SwitchTab,
    rename: RenameAttachedSession,
    detach: DetachSession,
    /// This feature's own repository — the two switches
    /// (`select_space`/`switch_tab`) call it directly, on `run`'s failure sink.
    terminal: Arc<dyn TerminalRepository + Send + Sync>,
    /// The attached session's id, or `None` for a bare `/terminal`; fixed for this mount.
    session_id: Option<String>,
    /// The sheet's `spaces` pill row.
    pub spaces: RwSignal<Vec<Space>>,
    /// The sheet's `tabs` pill row.
    pub tabs: RwSignal<Vec<TabEntry>>,
    /// The key of the tab the card is showing — the tab the *session* is on.
    pub active_tab: RwSignal<String>,
    /// The tree sheet's one read-only peek use case — see
    /// [`expand_space`](Self::expand_space).
    list_space_tabs: ListSpaceTabs,
    /// The tree sheet's expanded space, id-keyed (**never** index-keyed: the 3s
    /// layout poll + generation guard may republish [`tabs`](Self::tabs) mid-open) — at
    /// most one at a time, accordion semantics owned here rather than by the widget.
    /// `None` on zellij, which has no spaces to expand at all.
    pub expanded_space: RwSignal<Option<String>>,
    /// The tree sheet's expanded tab, id-keyed like
    /// [`expanded_space`](Self::expanded_space) — at most one at a time, against
    /// whichever tab list the expanded space is showing: the selected space's
    /// [`tabs`](Self::tabs), or a browsed space's
    /// [`browsed_space_tabs`](Self::browsed_space_tabs).
    pub expanded_tab: RwSignal<Option<String>>,
    /// The lazily fetched tabs of a browsed **non-selected** space, keyed by the space
    /// id they answer for — one slot is enough, since only one space is ever expanded
    /// at a time. The *selected* space's tabs keep coming from
    /// [`tabs`](Self::tabs); this never duplicates them.
    pub browsed_space_tabs: RwSignal<Option<(String, Vec<TabEntry>)>>,
    /// The active tab's lines — what the card's body prints.
    pub lines: RwSignal<Vec<TermLine>>,
    /// The attached session's current name, for the rename dialog's seed.
    pub session_name: RwSignal<Option<String>>,
    /// Whether the first load has landed (the card body stays empty until it has).
    pub loaded: RwSignal<bool>,
    /// Whether the card is fullscreen. Controller state, not widget state: it drives
    /// the card geometry, the expand button and the bar's hidden flag at once.
    pub fullscreen: RwSignal<bool>,
    /// The reveal replay token — see the [module docs](self).
    pub reveal_epoch: RwSignal<u64>,
    /// Mints [`reveal_epoch`](Self::reveal_epoch) values. An atomic, so a bump from a
    /// press handler (no reactive owner on desktop) never reads what it will write.
    next_epoch: AtomicU64,
    /// Whether the card shows the live pane instead of the mock transcript. Entered
    /// once per mount; there is no path back within the same mount.
    pub live: RwSignal<bool>,
    /// Which of the two the live pane is: `true` once [`attach_live`](Self::attach_live)
    /// claimed it for a real session, `false` for the fixture replay.
    live_attached: AtomicBool,
    /// Where a live session's stream stands, mirrored off the feed on the UI thread.
    pub link: RwSignal<Option<FeedStatus>>,
    /// The connection id last handed to the repository, so an unchanged one re-binds
    /// nothing. A `Mutex`, not a `Cell`: this type is `Send + Sync` under the bound.
    bound_connection_id: Mutex<String>,
    /// The live pane's cursor shape, from `AppearancePrefs::cursor_style`. Seeded with
    /// the shipped default so the first live frame is right before that read lands.
    pub cursor_style: RwSignal<term::CursorStyle>,
    /// The live pane's font size in px, from `AppearancePrefs::font_size_px`.
    pub font_size: RwSignal<f32>,
    /// The live pane's font stack, from `AppearancePrefs::font_family` via
    /// [`fonts::resolve_terminal_family`](crate::fonts::resolve_terminal_family).
    pub font_family: RwSignal<FontFamily>,
    /// The device-local preferences repository, resolved **once** by `pages.rs`'s
    /// factory — never re-resolved per poll tick, which would be ad-hoc DI.
    preferences: Arc<dyn PreferencesRepository + Send + Sync>,
    /// Test-only: how many [`refresh_appearance`](Self::refresh_appearance)
    /// ticks actually wrote a signal — pins that an unchanged poll writes
    /// nothing. Read via [`appearance_writes`](Self::appearance_writes).
    appearance_writes: AtomicU64,
    /// The staleness guard: bumped by
    /// [`begin_layout_fetch`](Self::begin_layout_fetch) before a round trip,
    /// re-checked before commit, so a slower fetch can't roll back a newer one.
    layout_generation: AtomicU64,
    /// Whether a poll-driven layout reload is outstanding — claimed by
    /// [`take_layout_poll_tick`](Self::take_layout_poll_tick), released by
    /// [`refresh_layout_from_poll`](Self::refresh_layout_from_poll), so a slow
    /// link never stacks round trips. Scoped to the poll only.
    layout_poll_in_flight: AtomicBool,
    /// Whether a create verb is in flight — the coalescing claim
    /// [`claim_create`](Self::claim_create) takes. A dropped press stays dropped.
    create_in_flight: AtomicBool,
    /// [`create_in_flight`](Self::create_in_flight)'s counterpart for every
    /// **move** ([`claim_move`](Self::claim_move)): the sheet's commit and the
    /// space switches alike. One slot for all of them rather than one apiece —
    /// two moves racing would leave the connection on one space with another
    /// space's tab world published.
    move_in_flight: AtomicBool,
    /// Whether the attached stream's handshake has settled —
    /// [`mirror_link`](Self::mirror_link) answers `true` on the wake that flips this.
    attach_settled: AtomicBool,
    /// Test-only counterpart of
    /// [`appearance_writes`](Self::appearance_writes), for layout fetches.
    layout_writes: AtomicU64,
    /// Where a **fresh** attach should land — the space/tab/pane the user
    /// last moved to, written only by user-driven moves
    /// (see [`remember_resume_pane`](Self::remember_resume_pane)).
    resume: Arc<ResumeSlot>,
    /// Whether the tree is the Tablet's docked rail rather than the modal sheet —
    /// see [`settle_rest`](Self::settle_rest).
    docked: AtomicBool,
}

/// The shipped default font size, px — settings' own `Preferences::defaults()`,
/// transcribed rather than reached for (this feature maps `settings`' values).
const DEFAULT_LIVE_FONT_SIZE: f32 = 12.0;

/// How often the live pane re-reads terminal-appearance prefs —
/// [`PreferencesRepository`] has no push seam. **3s**, matching `sessions`'
/// own cadence (transcribed, not imported: features don't share `presentation`).
const APPEARANCE_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// How often an attached live mount re-reads the tab/pane layout — the wire
/// has no layout-changed signal. **3s**, the original client's cadence: not
/// tighter, since RPCs share the attach stream's HTTP/2 connection and flow control.
const LAYOUT_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// How long a landed create waits before re-reading: the relay acks on
/// **enqueue**, not apply, so an immediate reload misses what was just made.
/// 350ms is the original client's own settle.
const CREATE_SETTLE: Duration = Duration::from_millis(350);

/// The sentinel [`CREATE_SETTLE_OVERRIDE_MS`] holds when no override is installed.
const NO_SETTLE_OVERRIDE: u64 = u64::MAX;

/// The settle a test installed with [`set_create_settle_for_tests`], in milliseconds.
static CREATE_SETTLE_OVERRIDE_MS: AtomicU64 = AtomicU64::new(NO_SETTLE_OVERRIDE);

/// Override [`CREATE_SETTLE`] for a headless test, so a suite can pin the post-create
/// reload's *ordering* without spending 350ms per create; `None` restores the shipped
/// value. `pub` rather than `#[cfg(test)]`: `tests/*.rs` link the normal build.
pub fn set_create_settle_for_tests(settle: Option<Duration>) {
    let value = settle.map_or(NO_SETTLE_OVERRIDE, |settle| settle.as_millis() as u64);
    CREATE_SETTLE_OVERRIDE_MS.store(value, Ordering::Relaxed);
}

/// The settle a create actually waits — [`CREATE_SETTLE`] unless a test overrode it.
fn create_settle() -> Duration {
    match CREATE_SETTLE_OVERRIDE_MS.load(Ordering::Relaxed) {
        NO_SETTLE_OVERRIDE => CREATE_SETTLE,
        millis => Duration::from_millis(millis),
    }
}

/// Whether a freshly read tab list re-seeds the active/browsed keys: a full
/// [`TerminalController::load`] re-seeds both outright, every partial reload only
/// **heals** a key that no longer names a tab, leaving a chosen browse put.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seed {
    /// Take the answer's own active tab, whatever the keys currently say.
    Fresh,
    /// Keep the current keys unless the answer no longer carries them.
    Heal,
}

/// Whether a layout fetch's failure is reported to the user.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Report {
    /// Emit on [`ControllerCore`]'s failure sink. Every user-initiated refresh.
    Toast,
    /// Swallow it: the poll runs unattended for the life of the mount, so a down link
    /// would toast on a cadence — the card's link strip already says so.
    Silent,
}

/// Map `settings`' own `CursorStyle` (reached via
/// [`entities::SettingsCursorStyle`](crate::features::terminal::domain::entities::SettingsCursorStyle))
/// onto this feature's `term::CursorStyle`. Variants line up 1:1.
fn map_cursor_style(style: SettingsCursorStyle) -> term::CursorStyle {
    match style {
        SettingsCursorStyle::Block => term::CursorStyle::Block,
        SettingsCursorStyle::Underline => term::CursorStyle::Underline,
        SettingsCursorStyle::Bar => term::CursorStyle::Bar,
    }
}

impl TerminalController {
    /// A fresh controller over the injected repositories, attached to `session_id`.
    pub fn new(
        terminal: Arc<dyn TerminalRepository + Send + Sync>,
        sessions: Arc<dyn SessionsRepository + Send + Sync>,
        preferences: Arc<dyn PreferencesRepository + Send + Sync>,
        session_id: Option<String>,
    ) -> Self {
        // The repository addresses whatever it is bound to; the connection id
        // only exists once a stream has minted one (see the module docs).
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
            session_name: RwSignal::new(None),
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
            layout_poll_in_flight: AtomicBool::new(false),
            create_in_flight: AtomicBool::new(false),
            move_in_flight: AtomicBool::new(false),
            attach_settled: AtomicBool::new(false),
            layout_writes: AtomicU64::new(0),
            resume: Arc::new(ResumeSlot::default()),
            docked: AtomicBool::new(false),
        }
    }

    /// This controller's [`ControllerCore`] — the failure sink the screen subscribes
    /// its toast bridge to. Named, since the [`AsRef`] impl's bare call is ambiguous.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// The attached session's id, if this screen was reached with one.
    pub fn session_id(&self) -> Option<String> {
        self.session_id.clone()
    }

    /// The resume hint every feed this screen opens attaches with — a second
    /// handle onto the same slot, not a copy, so a move recorded after the feed
    /// was built still rides its next reconnect. See [`ResumeSlot`].
    pub fn resume_slot(&self) -> Arc<ResumeSlot> {
        Arc::clone(&self.resume)
    }

    /// Load the whole screen (spaces, tabs, active tab's lines, session name)
    /// and play the reveal. Spaces are read only on a backend that has them
    /// ([`has_spaces`](Self::has_spaces)).
    ///
    /// **Tabs go through the layout staleness guard** like any other fetch
    /// ([`publish_tabs`](Self::publish_tabs)); everything else commits
    /// outside it, since nothing else reads that generation. The reverse
    /// race (spaces dropped by a faster reload) self-heals on the next space op.
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
        // Outside the tabs staleness guard below — see this method's own doc.
        self.spaces.set(spaces);
        if self.is_newest_layout_fetch(generation) {
            self.publish_tabs(tabs, Seed::Fresh);
        }
        self.lines.set(lines);
        self.session_name.set(session_name);
        self.loaded.set(true);
        self.replay_reveal();
        if self.is_docked() {
            self.settle_rest();
        }
    }

    /// Expand the space `id` in the tree sheet — [`expanded_space`](Self::expanded_space)'s
    /// one accordion writer. **Never `switch_space`**: browsing a space in the sheet must
    /// never move this connection's own selection, unlike [`select_space`](Self::select_space).
    ///
    /// The selected space's own tabs already live in [`tabs`](Self::tabs) — expanding it
    /// performs no RPC at all. Expanding any other space spawns a read-only
    /// [`ListSpaceTabs`] fetch, caching the answer under
    /// [`browsed_space_tabs`](Self::browsed_space_tabs). **Stale guard**: the answer is only
    /// published if [`expanded_space`](Self::expanded_space) still names `id` when the fetch
    /// lands — a user who has since expanded a different space (or collapsed this one) drops
    /// it silently. **Failure** rides the existing `ControllerCore::run` failure sink (one
    /// toast) and collapses the row back to nothing, itself guarded the same way.
    ///
    /// Switching which space is expanded also collapses
    /// [`expanded_tab`](Self::expanded_tab): a tab only ever belongs to the space showing it,
    /// so one left expanded under a space just closed is no longer meaningful.
    pub async fn expand_space(&self, id: String) {
        // The press that got here only reaches this body a pump later, by which
        // point the screen may already be gone — a disposed read panics.
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

    /// Collapse the tree sheet's expanded space — and the tab expanded under it, which
    /// belongs to no other space's list.
    pub fn collapse_space(&self) {
        self.expanded_space.set(None);
        self.expanded_tab.set(None);
    }

    /// Expand the pane list of the tab keyed `key` in the tree sheet. Synchronous and
    /// RPC-free like [`browse_tab`](Self::browse_tab): every pane rode in with its own tab,
    /// whichever list the expanded space is showing (the selected space's
    /// [`tabs`](Self::tabs), or a browsed space's
    /// [`browsed_space_tabs`](Self::browsed_space_tabs)) — picking the right one is the
    /// widget's read, not this write's concern.
    pub fn expand_tab(&self, key: String) {
        self.expanded_tab.set(Some(key));
    }

    /// Collapse the tree sheet's expanded tab, leaving the expanded space as it was.
    pub fn collapse_tab(&self) {
        self.expanded_tab.set(None);
    }

    /// Seed the tree sheet's resting expansion on open: the selected space (herdr only —
    /// zellij's top level is tabs, so it has none to expand) and the active tab both open
    /// pre-expanded, so the sheet lands on the tab the session is on without the user
    /// drilling to it; playing no opening animation for either is the widget's own
    /// concern, not this write's.
    pub fn seed_expansion(&self) {
        if self.is_herdr_session() {
            self.expanded_space.set(self.selected_space_id());
        }
        self.expanded_tab.set(Some(self.active_tab.get_untracked()));
    }

    /// Mark the tree docked (the Tablet rail) or not, answering whether that changed.
    pub fn set_docked(&self, docked: bool) -> bool {
        self.docked.swap(docked, Ordering::Relaxed) != docked
    }

    /// Whether the tree is the docked rail — see [`set_docked`](Self::set_docked).
    pub fn is_docked(&self) -> bool {
        self.docked.load(Ordering::Relaxed)
    }

    /// Put the docked rail back in its **resting** state: the selected space and the tab
    /// the server flags active both expanded, nothing browsed. A landed load, a landed
    /// commit and a browse closed under another space all come back here, so the rail
    /// always shows where the session is. A no-op on a disposed mount.
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

    /// Close the expanded space `id`: docked, a browsed space hands the rail back to
    /// [`settle_rest`](Self::settle_rest); anything else collapses as the sheet does.
    pub fn close_space(&self, id: &str) {
        if self.is_docked() && self.selected_space_id().as_deref() != Some(id) {
            self.settle_rest();
        } else {
            self.collapse_space();
        }
    }

    /// Clear the tree sheet's whole browse: expansion and the browsed-space cache. The
    /// sheet's dismiss calls this, so nothing survives to the next open, which reseeds via
    /// [`seed_expansion`](Self::seed_expansion) instead of resuming a stale browse.
    pub fn reset_browse(&self) {
        self.expanded_space.set(None);
        self.expanded_tab.set(None);
        self.browsed_space_tabs.set(None);
    }

    /// Move the session to tab `key`: focus it, replace the body's lines,
    /// replay the reveal — the tab step of [`commit_pane`](Self::commit_pane)
    /// (its one caller). The stream stays open across it: herdr re-points
    /// this connection's relay in place, so dropping it would lose that retarget.
    pub async fn switch_tab(&self, key: String) -> bool {
        let Some(tab_id) = self.tab_id_for(&key) else {
            return false;
        };
        // The focus move first: a switch the backend refused must not leave the
        // card showing a tab the session is not on.
        if !self.tell(self.terminal.go_to_tab(tab_id)).await {
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

    /// Commit the sheet's browse: focus pane `pane_id` of tab `tab_key`
    /// (`is_plugin` picks the wire namespace) — the same-space path, unchanged;
    /// [`commit_pane_in_space`](Self::commit_pane_in_space) is the sibling for a row
    /// under a space other than the one this connection is on. Order: tab move first (a
    /// pane only focuses inside the focused tab), then pane focus on every backend —
    /// herdr wire-swaps this connection's relay in place, so a reconnect's resume hint is
    /// only a best-effort ask. A refused pane focus after a landed tab move still keeps
    /// that move. **Single-flight** ([`claim_move`](Self::claim_move)): a tap in
    /// flight is dropped, not queued — and so is one arriving while a space switch
    /// holds that same claim. **Accepted residual**: no re-read behind the move —
    /// a racing poll may transiently show the relay's own focus, self-correcting within
    /// one [`LAYOUT_POLL_INTERVAL`] tick. The badge write is deliberately *not* fenced
    /// with [`begin_layout_fetch`](Self::begin_layout_fetch): a same-space commit leaves
    /// the tab world itself alone, so an older fetch landing behind it republishes the
    /// same tabs and only the focus flag wavers. A cross-space promotion is the opposite
    /// case, and does fence — see
    /// [`promote_browsed_tabs`](Self::promote_browsed_tabs).
    pub async fn commit_pane(&self, tab_key: String, pane_id: u32, is_plugin: bool) -> bool {
        let Some(_claim) = self.claim_move() else {
            return false;
        };
        self.commit_pane_unclaimed(tab_key, pane_id, is_plugin)
            .await
    }

    /// [`commit_pane`](Self::commit_pane)'s body, without its claim —
    /// [`commit_pane_in_space`](Self::commit_pane_in_space)'s cross-space branch holds
    /// that claim across the `switch_space` step ahead of this, so it calls straight
    /// in rather than re-claiming what it already holds. See
    /// [`commit_pane`](Self::commit_pane) for the tab/pane ordering contract.
    ///
    /// **Teardown-safe**: every signal *read* on this path is `try_*`, here and in
    /// [`tab_id_for`](Self::tab_id_for)/[`focus_pane_locally`](Self::focus_pane_locally).
    /// The tap that got here resumes a pump later — on the cross-space path, a whole
    /// switch round trip later — and reading a disposed owner panics (the writes
    /// beside those reads are no-ops). A commit whose screen is gone answers `false`
    /// and moves nothing.
    async fn commit_pane_unclaimed(&self, tab_key: String, pane_id: u32, is_plugin: bool) -> bool {
        let Some(active) = self.active_tab.try_get_untracked() else {
            return false;
        };
        if tab_key != active && !self.switch_tab(tab_key.clone()).await {
            return false;
        }
        if !self
            .tell(self.terminal.focus_pane(pane_id, is_plugin))
            .await
        {
            return false;
        }
        self.focus_pane_locally(&tab_key, pane_id, is_plugin);
        if self.is_herdr_session() {
            self.remember_resume_pane(&tab_key, pane_id);
        }
        // No reload behind this: the optimistic badge above is the feedback,
        // and the layout poll converges on the server's own focus — a reload
        // here could land before the enqueue-ack applies and revert the badge.
        true
    }

    /// Commit the tree sheet's browse when the tapped pane's tab belongs to a space
    /// other than the one this connection is on: [`switch_space`](TerminalRepository::switch_space)
    /// first (recording the resume hint exactly as [`select_space`](Self::select_space)
    /// does), then the same tab+pane steps [`commit_pane`](Self::commit_pane) takes. A
    /// `space_id` that is already selected takes
    /// [`commit_pane`](Self::commit_pane)'s own path, byte-identical — this is only the
    /// sibling for a row under a **non-selected** space.
    ///
    /// The target space's tab world comes from the browse that got here in the first
    /// place — [`expand_space`](Self::expand_space)'s cached
    /// [`browsed_space_tabs`](Self::browsed_space_tabs) — promoted onto
    /// [`tabs`](Self::tabs)/[`active_tab`](Self::active_tab) rather than re-read: the
    /// sheet never draws a cross-space row without having already fetched it.
    ///
    /// **Fail-closed on a lost cache**: that promotion is read **before the first
    /// await**, since the tapped row was rendered from it. With nothing cached for
    /// `space_id` the commit answers `false` having sent nothing at all — resolving
    /// `tab_key` against the space this connection is still on would focus a pane the
    /// user never pointed at, and a half-landed `switch_space` would strand it there.
    ///
    /// **The selection follows the switch** ([`publish_space_switch`](Self::publish_space_switch)):
    /// [`spaces`](Self::spaces) is what the tree keys its per-space tab world off and what
    /// [`expand_space`](Self::expand_space) short-circuits on, so a landed switch that
    /// left it naming the old space would misdraw every later open of the sheet.
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
        let Some(_claim) = self.claim_move() else {
            return false;
        };
        let Some(cached_tabs) = self.browsed_tabs_for(&space_id) else {
            return false;
        };
        if !self
            .tell(self.terminal.switch_space(space_id.clone()))
            .await
        {
            return false;
        }
        if self.is_herdr_session() {
            self.resume.remember_space(space_id.clone());
        }
        self.select_space_locally(&space_id);
        self.promote_browsed_tabs(cached_tabs);
        let landed = self
            .commit_pane_unclaimed(tab_key, pane_id, is_plugin)
            .await;
        // Behind the pane steps, not ahead of them: the switch landed either
        // way, so the space list converges either way — and a user waiting on a
        // pane focus never waits on this read.
        self.publish_space_switch(&space_id).await;
        landed
    }

    /// The tabs the tree cached for `space_id`, if that peek is still the one on hand
    /// — [`commit_pane_in_space`](Self::commit_pane_in_space)'s entry read, and the
    /// only thing it trusts a cross-space `tab_key` against. `None` covers both a
    /// cache cleared since the row was drawn ([`reset_browse`](Self::reset_browse))
    /// and one that has since moved to another space.
    fn browsed_tabs_for(&self, space_id: &str) -> Option<Vec<TabEntry>> {
        match self.browsed_space_tabs.try_get_untracked()? {
            Some((cached_space, cached_tabs)) if cached_space == space_id => Some(cached_tabs),
            _ => None,
        }
    }

    /// Promote a browsed space's cached tab world onto [`tabs`](Self::tabs) and
    /// [`active_tab`](Self::active_tab) — what a landed cross-space switch owes the
    /// card and the sheet, without a re-read the browse already paid for.
    ///
    /// **Fenced** ([`begin_layout_fetch`](Self::begin_layout_fetch)): this promotion is
    /// itself the newest word on the tab world, so it outdates every fetch already in
    /// flight. A poll issued *before* the switch would otherwise still hold the newest
    /// generation and heal these two signals straight back to the space just left —
    /// while [`spaces`](Self::spaces) names the new one — leaving the sheet and the
    /// card disagreeing until the next [`LAYOUT_POLL_INTERVAL`] tick, with every tap in
    /// that window aimed at the wrong space's tabs.
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

    /// Move the selection onto the space `id` in this screen's own state — the
    /// optimistic half of a landed `switch_space`, the same posture
    /// [`focus_pane_locally`](Self::focus_pane_locally) takes for a pane badge.
    /// Read-compare-then-write, and a no-op on a disposed mount.
    fn select_space_locally(&self, id: &str) {
        let Some(spaces) = self.spaces.try_get_untracked() else {
            return;
        };
        if spaces.iter().any(|space| space.selected && space.id == id) {
            return;
        }
        self.spaces.set(select_by_id(spaces, id));
    }

    /// Publish a landed `switch_space`: the selection moves **now**
    /// ([`select_space_locally`](Self::select_space_locally)), then the space list is
    /// re-read to converge whatever else moved. The re-read keeps `id` selected —
    /// `SwitchSpace` acks on enqueue, so a list can still answer from before the relay
    /// applied it, and nothing else re-lists spaces to correct it afterwards (the 3s
    /// poll reads tabs alone).
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

    /// Focus pane `pane_id` of the live session's active tab — the
    /// content-tap hook's route, never wired for herdr. Unlike
    /// [`commit_pane`](Self::commit_pane), a tap never crosses tabs, so this
    /// skips the switch step and reloads on landing to converge the tap's
    /// rects with the sheet's badge. A refused call writes nothing.
    pub async fn focus_pane_from_tap(&self, pane_id: u32, is_plugin: bool) -> bool {
        let tab_key = self.active_tab.get_untracked();
        if !self
            .tell(self.terminal.focus_pane(pane_id, is_plugin))
            .await
        {
            return false;
        }
        self.focus_pane_locally(&tab_key, pane_id, is_plugin);
        self.reload_tabs().await;
        true
    }

    /// Move the focused flag onto the pane `pane_id` of the tab keyed `tab_key`, in
    /// this screen's own state — the optimistic half of
    /// [`commit_pane`](Self::commit_pane). One focused pane per tab whatever layer it
    /// is on: the wire flags focus per layer, but the sheet shows one badge. A no-op
    /// on a disposed mount, like every other read this commit path takes.
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

    /// Record the pane of `tab_key` as where a **fresh** attach should land —
    /// written only from this user-driven path, never from
    /// [`publish_tabs`](Self::publish_tabs) or the poll: a layout read
    /// reports the server's *current* focus, which right after reconnect is
    /// the daemon-global one, and would overwrite the hint it exists to correct.
    fn remember_resume_pane(&self, tab_key: &str, pane_id: u32) {
        self.resume.remember_pane(self.tab_id_for(tab_key), pane_id);
    }

    /// Make the space `id` active and reload the tab world behind it — a
    /// space *is* a different set of tabs. `SwitchSpace` carries no read-only
    /// gate on the wire, so a read-only credential reaches this the same as
    /// any other caller; the only refusal is a fail-closed `reattach
    /// required`, already surfaced as `Err` here. Answers whether the switch
    /// landed.
    ///
    /// A landed switch also becomes the resume hint's space rung — the one
    /// part of it a muxrd restart cannot stale — and moves
    /// [`spaces`](Self::spaces)' own selected flag twice: once immediately, so
    /// the sheet is right during the reload's round trips, and once behind
    /// [`load`](Self::load) in case the list it read still answered from
    /// before the relay applied the switch. Both are no-ops once the list
    /// already agrees ([`select_space_locally`](Self::select_space_locally)).
    ///
    /// **Single-flight** ([`claim_move`](Self::claim_move)): a switch arriving while
    /// another move — a switch or the sheet's commit — is in flight is dropped whole,
    /// sending nothing at all, since a half-applied second switch is exactly how the
    /// tab world and the space list come apart.
    pub async fn select_space(&self, id: String) -> bool {
        let Some(_claim) = self.claim_move() else {
            return false;
        };
        self.select_space_unclaimed(id).await
    }

    /// [`select_space`](Self::select_space)'s body, without its claim — the entries
    /// already holding one ([`new_pane_in`](Self::new_pane_in) through
    /// [`adopt_space_unclaimed`](Self::adopt_space_unclaimed)) call straight in rather
    /// than re-claiming what they are holding, the same shape
    /// [`commit_pane_unclaimed`](Self::commit_pane_unclaimed) takes.
    async fn select_space_unclaimed(&self, id: String) -> bool {
        if !self.tell(self.terminal.switch_space(id.clone())).await {
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

    /// Make `space_id` the space this connection is on, if it is not already —
    /// what the tree's cross-space **create** rows take before creating
    /// anything, since every create lands in the selected space. Answers
    /// whether the screen is on that space now.
    ///
    /// This is also the one way an **empty** space is reached at all: it draws
    /// no pane row to commit through, so its add-tab row's switch is its only
    /// entry.
    ///
    /// A switch it actually has to make rides
    /// [`select_space`](Self::select_space)'s own single-flight claim, and answers
    /// `false` when another move holds it — so a losing tap opens no dialog either.
    /// Already being on `space_id` needs no claim: there is nothing to move.
    pub async fn adopt_space(&self, space_id: String) -> bool {
        if self.selected_space_id().as_deref() == Some(space_id.as_str()) {
            return true;
        }
        self.select_space(space_id).await
    }

    /// [`adopt_space`](Self::adopt_space) for a caller already holding the move claim
    /// — see [`select_space_unclaimed`](Self::select_space_unclaimed).
    async fn adopt_space_unclaimed(&self, space_id: String) -> bool {
        if self.selected_space_id().as_deref() == Some(space_id.as_str()) {
            return true;
        }
        self.select_space_unclaimed(space_id).await
    }

    /// Create a new space named `label` (already trimmed by the dialog) and pull the
    /// sheet's space list up to date. The wire's ack carries no id, so this re-lists
    /// the spaces and, on a name match, flags that one selected locally; a non-match
    /// leaves the server's answer standing. Not a full [`load`](Self::load) — creating
    /// a space touches no tab — but the re-list still waits out [`CREATE_SETTLE`], and
    /// repeated presses coalesce ([`claim_create`](Self::claim_create)).
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

    /// Open a new tab named `name` (empty creates **unnamed**, the wire's own default
    /// naming) and reload the workspace so it appears. A full [`load`](Self::load),
    /// unlike [`create_space`](Self::create_space): the server focuses what it just
    /// opened, so `load`'s re-seed puts the browse on it. Waits out [`CREATE_SETTLE`];
    /// repeated presses coalesce.
    pub async fn new_tab(&self, name: String) {
        let Some(_claim) = self.claim_create() else {
            return;
        };
        if self.tell(self.terminal.new_tab(name)).await {
            self.settle_create().await;
            self.load().await;
        }
    }

    /// Open a new pane under the tab the tree's add-pane row sits on, wherever that
    /// tab lives: switch to its space when it is not the selected one
    /// ([`adopt_space`](Self::adopt_space)), move the session onto the tab
    /// ([`switch_tab`](Self::switch_tab)), then create. Every create lands in the tab
    /// the session is on, so a row under a merely-browsed tab has to move first or the
    /// pane appears somewhere the user never pointed. `space_id` is `None` on a backend
    /// with no spaces, and a step that does not land stops the rest.
    ///
    /// **Single-flight over the whole sequence** ([`claim_move`](Self::claim_move)), not
    /// just over the create at the end of it: a second row's tap that only lost
    /// [`claim_create`](Self::claim_create) would still have moved the session to its
    /// own space and tab first, stranding the pane the winning tap created somewhere the
    /// user never pointed — and saying nothing about it. The losing tap now sends
    /// nothing at all.
    pub async fn new_pane_in(&self, space_id: Option<String>, tab_key: String, floating: bool) {
        let Some(_claim) = self.claim_move() else {
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

    /// Open a new pane in the tab the session is on — tiled (`floating = false`) or
    /// floating. A backend that ignores the flag (herdr does) still receives it.
    /// Reloads the workspace on landing, the same shape as [`new_tab`](Self::new_tab).
    pub async fn new_pane(&self, floating: bool) {
        let Some(_claim) = self.claim_create() else {
            return;
        };
        if self.tell(self.terminal.new_pane(floating)).await {
            self.settle_create().await;
            self.load().await;
        }
    }

    /// Rename the space with [`Space::id`] `space_id` to `label` and pull the sheet's
    /// space list up to date. Not a full [`load`](Self::load) (see
    /// [`create_space`](Self::create_space)); no re-select — an id survives a rename.
    pub async fn rename_space(&self, space_id: String, label: String) {
        if !self.tell(self.terminal.rename_space(space_id, label)).await {
            return;
        }
        match self.terminal.list_spaces().await {
            Ok(spaces) => self.spaces.set(spaces),
            Err(failure) => self.core.failures().emit(&failure),
        }
    }

    /// Rename the tab with [`TabEntry::tab_id`] `tab_id` to `name`, then pull the tab
    /// list up to date via [`reload_tabs`](Self::reload_tabs). Not a full
    /// [`load`](Self::load): a rename creates nothing for the server to refocus.
    pub async fn rename_tab(&self, tab_id: u64, name: String) {
        if self.tell(self.terminal.rename_tab(tab_id, name)).await {
            self.reload_tabs().await;
        }
    }

    /// Rename the pane `pane_id` to `name`, then reload the tab list the way
    /// [`rename_tab`](Self::rename_tab) does — a title rides in [`TabEntry::panes`].
    pub async fn rename_pane(&self, pane_id: u32, is_plugin: bool, name: String) {
        if self
            .tell(self.terminal.rename_pane(pane_id, is_plugin, name))
            .await
        {
            self.reload_tabs().await;
        }
    }

    /// Scroll the pane `pane_id` one wheel notch in `direction` — the wheel hook's RPC
    /// fallback for a server with no `mouse-input` capability. Fire-and-forget,
    /// deliberately off the failure sink: a drag emits one call per line, so toasting
    /// a server that refuses every one would spam the whole drag.
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

    /// Re-list the tabs and publish them — the partial refresh every trigger but the
    /// mount's own load shares. None creates anything the server might refocus, so
    /// re-reading [`tabs`](Self::tabs) alone is enough, and a key that no longer names
    /// a tab heals itself on the way through ([`Seed::Heal`]).
    pub async fn reload_tabs(&self) {
        self.refresh_layout(Report::Toast).await;
    }

    /// [`reload_tabs`](Self::reload_tabs)'s body, with the caller's failure policy.
    async fn refresh_layout(&self, report: Report) {
        let generation = self.begin_layout_fetch();
        match self.terminal.list_tabs().await {
            Ok(tabs) => {
                if self.is_newest_layout_fetch(generation) {
                    self.publish_tabs(tabs, Seed::Heal);
                }
            }
            Err(failure) => {
                if report == Report::Toast {
                    self.core.failures().emit(&failure);
                }
            }
        }
    }

    /// **Consume** one layout poll tick: skips if the first load hasn't
    /// landed, or the previous tick is still outstanding. The caller releases
    /// the claim via [`refresh_layout_from_poll`](Self::refresh_layout_from_poll).
    pub fn take_layout_poll_tick(&self) -> bool {
        self.loaded.get_untracked() && !self.layout_poll_in_flight.swap(true, Ordering::SeqCst)
    }

    /// The refresh a claimed poll tick runs: [`reload_tabs`](Self::reload_tabs)
    /// with failures swallowed, releasing
    /// [`take_layout_poll_tick`](Self::take_layout_poll_tick)'s claim via an
    /// RAII guard, so an early return cannot wedge the poll shut.
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

    /// Open a fresh layout fetch and answer its generation — the staleness
    /// guard every fetch shares (see [`layout_generation`](Self::layout_generation)).
    fn begin_layout_fetch(&self) -> u64 {
        self.layout_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether the fetch that took `generation` may still commit its answer.
    fn is_newest_layout_fetch(&self, generation: u64) -> bool {
        self.layout_generation.load(Ordering::SeqCst) == generation
    }

    /// Publish a freshly read tab list, **read-compare-then-write** with the
    /// active key reconciled per `seed` — the one commit point every layout
    /// fetch goes through.
    ///
    /// Nothing unchanged is written: `tabs` is a tracked read polled for the
    /// mount's life, so an unconditional `set` would rebuild forever on an
    /// idle session. A write bumps [`layout_writes`](Self::layout_writes).
    /// **Disposal-safe**: a tick in flight at teardown reads `try_*`, so the
    /// commit is a no-op.
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
        // Whichever tab the answer flags active, falling back to its first.
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
        // Docked, the rail follows a focus another client moved — but only when it was
        // resting on the old one: a browse in progress is the user's, not the poll's.
        if self.is_docked() && moved != resting && self.expanded_tab.get_untracked() == resting {
            self.expanded_tab.set(moved);
        }
        if wrote {
            self.layout_writes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Whether this controller's reactive state is still alive — `false` once the mount
    /// has been disposed. The signals share one owner, so one answer stands for all.
    ///
    /// Untracked, so asking never subscribes. Public because the tree sheet is a page of
    /// the **root** overlay host: it can outlive the screen that pushed it, and a build
    /// reading a disposed signal panics.
    pub fn signals_alive(&self) -> bool {
        self.tabs.try_with_untracked(|_| ()).is_some()
    }

    /// Claim the one in-flight create slot, or `None` when a create is already running.
    /// The claim releases on drop, so a refused create re-arms the affordance.
    fn claim_create(&self) -> Option<FlightClaim<'_>> {
        (!self.create_in_flight.swap(true, Ordering::SeqCst))
            .then(|| FlightClaim(&self.create_in_flight))
    }

    /// [`claim_create`](Self::claim_create)'s counterpart for a **move** — the same
    /// drop-not-queue idiom over one slot shared by the sheet's commit
    /// ([`commit_pane`](Self::commit_pane)/[`commit_pane_in_space`](Self::commit_pane_in_space))
    /// and every space switch ([`select_space`](Self::select_space)/
    /// [`adopt_space`](Self::adopt_space)/[`new_pane_in`](Self::new_pane_in)). They
    /// exclude each other, not just their own kind: a switch that landed while a
    /// commit was mid-flight would strand that commit's tab/pane steps in a space
    /// the user never pointed at.
    fn claim_move(&self) -> Option<FlightClaim<'_>> {
        (!self.move_in_flight.swap(true, Ordering::SeqCst))
            .then(|| FlightClaim(&self.move_in_flight))
    }

    /// Wait out [`CREATE_SETTLE`] between a create's ack and the reload behind it.
    async fn settle_create(&self) {
        clean_signals::time::sleep(create_settle()).await;
    }

    /// Test/diagnostic-only accessor for [`layout_writes`](Self::layout_writes).
    pub fn layout_writes(&self) -> u64 {
        self.layout_writes.load(Ordering::Relaxed)
    }

    /// Whether this session's backend has **spaces** — the one fact the sheet gates its
    /// spaces section on. Only herdr does; zellij's `GetSpaces` answers an empty list.
    pub fn has_spaces(&self) -> bool {
        self.is_herdr_session()
    }

    /// Whether the attached session runs on herdr — the discriminator itself, spelled
    /// once so no call site re-parses the id. Answers off
    /// [`session_id`](Self::session_id): which backend a session runs on is a fact
    /// about the session, not about whether a stream is open onto it.
    pub fn is_herdr_session(&self) -> bool {
        self.session_id.as_deref().is_some_and(is_herdr)
    }

    /// The backend id of the tab keyed `key`, if the sheet is still showing it —
    /// `None` once the mount is gone too, so a resumed commit exits quietly rather
    /// than panicking on a disposed owner.
    fn tab_id_for(&self, key: &str) -> Option<u64> {
        self.tabs
            .try_get_untracked()?
            .into_iter()
            .find(|tab| tab.key == key)
            .map(|tab| tab.tab_id)
    }

    /// The id of the currently selected space, from [`spaces`](Self::spaces) — `None`
    /// on a backend with none (zellij), before the first load has landed, or once the
    /// mount is gone (this is the entry read of a commit a spawned tap resumes, so a
    /// screen can already have been torn down under it).
    fn selected_space_id(&self) -> Option<String> {
        self.spaces
            .try_get_untracked()?
            .into_iter()
            .find(|space| space.selected)
            .map(|space| space.id)
    }

    /// Drive one repository call that hands nothing back, reporting whether it landed
    /// and emitting whatever it did not on `ControllerCore::run`'s failure sink.
    async fn tell(&self, work: impl Future<Output = Result<(), AppFailure>>) -> bool {
        match work.await {
            Ok(()) => true,
            Err(failure) => {
                self.core.failures().emit(&failure);
                false
            }
        }
    }

    /// Rename the attached session to `name`, writing through the **sessions**
    /// repository so the list reflects it. Answers the stored name, for the toast.
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

    /// Detach from the attached session; answers whether the call landed.
    pub async fn detach(&self) -> bool {
        let Some(id) = self.session_id.clone() else {
            return false;
        };
        self.core
            .run(&self.detach, id, RunOptions::default())
            .await
            .is_ok()
    }

    /// Flip fullscreen and answer the new state (the caller publishes it to the bar).
    pub fn toggle_fullscreen(&self) -> bool {
        let next = !self.fullscreen.get_untracked();
        self.fullscreen.set(next);
        next
    }

    /// The tab the card is showing, if the load has landed.
    pub fn active_tab_entry(&self) -> Option<TabEntry> {
        let key = self.active_tab.get_untracked();
        self.tabs
            .get_untracked()
            .into_iter()
            .find(|tab| tab.key == key)
    }

    /// Bump the reveal replay token: one rebuild per replay, zero per frame.
    fn replay_reveal(&self) {
        self.reveal_epoch
            .set(self.next_epoch.fetch_add(1, Ordering::Relaxed));
    }

    /// Enter the live pane over the **checked-in fixture replay**: with the overflow
    /// menu's *Demo session* row withdrawn, this is the seam `tests/terminal_live.rs`
    /// drives the real grid through. Idempotent, and never attaches to a real session
    /// ([`attach_live`](Self::attach_live) is that entry). [`LiveSession`] itself is
    /// built by `build` — see [`LiveSession::enter`]'s owner contract.
    pub fn enter_live(&self) {
        self.live.set(true);
    }

    /// Enter the live pane **attached to this screen's own `muxrd` session** — what a
    /// connected `/terminal?session=<id>` mounts. Claims the pane before flipping the
    /// flag, so the first `build` that observes it picks the real feed.
    pub fn attach_live(&self) {
        self.live_attached.store(true, Ordering::Relaxed);
        self.live.set(true);
    }

    /// The session the live pane should attach to, or `None` for the demo replay —
    /// what [`LiveSession::enter`] branches its feed on.
    pub fn attached_session(&self) -> Option<String> {
        if self.live_attached.load(Ordering::Relaxed) {
            self.session_id.clone()
        } else {
            None
        }
    }

    /// Mirror one poll of the attached feed onto the UI thread: publish its
    /// status, and re-bind the repository when the stream mints a new
    /// connection id, so later requests route to this client's view.
    ///
    /// **Read-compare-then-write**: [`link`](Self::link) is a tracked read in
    /// `build`, so an unconditional `set` would wake a rebuild 30x/s.
    ///
    /// Answers `true` on the first wake with a real connection id — the
    /// signal the caller uses to fire a catch-up
    /// [`reload_tabs`](Self::reload_tabs), since the mount's own load went
    /// out with no connection id to route by.
    pub fn mirror_link(&self, status: FeedStatus, connection_id: Option<String>) -> bool {
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
        let settled = !connection_id.is_empty() || status == FeedStatus::Connected;
        settled
            && self
                .attach_settled
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
    }

    /// Re-read the terminal-appearance prefs and update
    /// [`cursor_style`](Self::cursor_style)/[`font_size`](Self::font_size)/
    /// [`font_family`](Self::font_family) — once on entering the live pane,
    /// then on [`APPEARANCE_POLL_INTERVAL`]'s cadence. A failed read leaves
    /// the signals as they were.
    ///
    /// **Read-compare-then-write**: all three are tracked reads in `build`,
    /// so an unconditional write would wake a rebuild every idle tick; only a
    /// changed value is `set()`. **Disposal-safe**: each tick spawns detached
    /// from the owner, so the first read is a `try_get_untracked` — `None`
    /// stands for the whole controller (one shared owner).
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

    /// Test/diagnostic-only accessor for [`appearance_writes`](Self::appearance_writes).
    pub fn appearance_writes(&self) -> u64 {
        self.appearance_writes.load(Ordering::Relaxed)
    }
}

/// The in-flight claim [`TerminalController::claim_create`] and
/// [`TerminalController::claim_move`] hand out — released on drop, so it covers
/// the whole verb (for a create: its ack, settle and reload) however that verb
/// returns.
struct FlightClaim<'a>(&'a AtomicBool);

impl Drop for FlightClaim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The backend prefix of a `<backend>:<name>` session id — see [`is_herdr`].
const HERDR_BACKEND: &str = "herdr:";

/// Drop the current `AttachTerminal` stream and open a fresh one at the
/// grid's current size — [`LiveSession::reattach`]'s shareable form,
/// **recovery only**: unlike an in-place focus verb, a fresh attach's resume
/// hint is best-effort, so a herdr session risks landing on the daemon's
/// global focus. A closure (not a method) since it owns the [`AttachSpec`].
pub type Reattach = Rc<dyn Fn()>;

/// The key of the tab `tabs` flags active — where the server says focus is, and so
/// where the docked rail rests.
fn rest_tab_key(tabs: &[TabEntry]) -> Option<String> {
    tabs.iter()
        .find(|tab| tab.active)
        .map(|tab| tab.key.clone())
}

/// Whether the session with id `session` runs on the **herdr** backend — this screen's
/// one backend discriminator, read off the id's `<backend>:<name>` convention
/// (`SessionInfo.id`), the same fact `sessions` reconciles a rename against.
fn is_herdr(session: &str) -> bool {
    session.starts_with(HERDR_BACKEND)
}

/// Flag the space with [`Space::id`] `id` selected among `spaces`, clearing every other
/// flag — the id-keyed twin of [`select_by_label`], and what a **landed** `switch_space`
/// writes: the switch acked, so this client's own ask outranks a list that may still
/// answer from before the relay applied it. A non-match changes nothing, which keeps a
/// space list that has since dropped `id` from ending up with nothing selected at all.
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

/// Flag the space named `label` selected among `spaces`, leaving every other one's flag
/// as the server answered — the local half of
/// [`TerminalController::create_space`]'s courtesy pick. A non-match changes nothing.
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

/// The live pane's byte source, held **concretely** so the half not on the
/// [`TerminalFeed`] port stays reachable (demo test hooks; a real session's
/// status/connection-id/backoff). Byte-only callers take [`shared`](Self::shared).
pub enum LiveFeed {
    /// The checked-in fixture replay.
    Demo(Rc<RefCell<DemoFeed>>),
    /// A real `muxrd` session's `AttachTerminal` stream.
    Grpc(Rc<RefCell<GrpcTerminalFeed>>),
}

impl LiveFeed {
    /// This feed as the trait object the pump and the input seam take: a second handle
    /// onto the *same* cell, not a copy, so a re-attach through the concrete handle is
    /// visible through every trait object minted here.
    fn shared(&self) -> term::SharedFeed {
        match self {
            LiveFeed::Demo(feed) => feed.clone(),
            LiveFeed::Grpc(feed) => feed.clone(),
        }
    }

    /// The demo replay behind this feed, if that is what it is — `pages::LiveProbe`'s handle.
    pub fn demo(&self) -> Option<&Rc<RefCell<DemoFeed>>> {
        match self {
            LiveFeed::Demo(feed) => Some(feed),
            LiveFeed::Grpc(_) => None,
        }
    }

    /// Where a real session's stream stands, or `None` for the demo replay.
    pub fn status(&self) -> Option<FeedStatus> {
        match self {
            LiveFeed::Demo(_) => None,
            LiveFeed::Grpc(feed) => Some(feed.borrow().status()),
        }
    }
}

/// Everything the live pane needs: emulator, feed, input/selection seams,
/// and the pump's repaint counter — built once on entering the pane
/// ([`LiveSession::enter`]), dropped on unmount. Not a [`TerminalController`]
/// field: every handle is `Rc`-backed (event-pass mutation), which would
/// make the controller `!Send`/`!Sync`.
///
/// **Dropping it is the detach**: [`GrpcTerminalFeed`]'s `Drop` shuts the
/// request stream, so unmount is what the server sees as this client leaving.
pub struct LiveSession {
    /// The VT emulator the grid renders and the selection resolves against.
    pub emulator: term::SharedEmulator,
    /// The byte source — see [`LiveFeed`].
    pub feed: LiveFeed,
    /// The grid's (and the key bar's) shared keystroke source.
    pub input: term::InputSeam,
    /// The grid's (and the Copy chip's) shared selection gesture.
    pub selection: term::SelectionSeam,
    /// The feed pump's repaint counter — a **tracked** read of this in
    /// `Component::build` is what wakes the live pane's repaint (see
    /// `presentation::term`'s ~30 Hz coalescing contract).
    pub generation: RwSignal<u64>,
    /// Drop this session's stream and open a fresh one at the grid's current size —
    /// `None` for the demo replay. See [`Reattach`] for why this is recovery alone.
    pub reattach: Option<Reattach>,
}

/// What re-opening an attached stream needs, beyond the feed handle itself.
struct AttachSpec {
    connection: Arc<ConnectionController>,
    session: String,
    /// Fires on the **background** runtime, where writing a signal is
    /// forbidden — it only raises a flag; the UI-thread poll tick turns
    /// that into the teardown.
    auth_expired: Arc<AtomicBool>,
    /// The controller's resume slot, handed to every feed this spec opens.
    resume: Arc<ResumeSlot>,
}

impl LiveSession {
    /// Construct the seams + feed and start the feed pump and appearance poll.
    ///
    /// **Must run under the screen's own component owner** (`Component::build`
    /// only, never a press handler — no owner on desktop), so
    /// `spawn_feed_pump`'s/`use_interval`'s `on_cleanup` bind and fire on
    /// unmount. `pages::TerminalScreen::build` guards a once-per-mount call.
    ///
    /// **Which feed**: [`TerminalController::attached_session`] ANDed with a
    /// live connection client — anything else falls back to the fixture
    /// replay rather than panicking.
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
        // Wires the seam's echo drain to this session's emulator/generation pair:
        // without it a typed keystroke waits on `run_feed`'s next wake.
        input.wire_pump(Rc::clone(&emulator), generation);
        let selection = term::SelectionSeam::new(Rc::clone(&emulator));

        let reattach = match (&feed, spec) {
            (LiveFeed::Grpc(grpc), Some(spec)) => {
                // The status/id/auth poll rides the repaint pump: already awake,
                // already on the UI thread, at the rate the pane repaints.
                let watch = Rc::clone(grpc);
                let mirror = Arc::clone(controller);
                let expiry = Arc::clone(&spec.connection);
                let flag = Arc::clone(&spec.auth_expired);
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
                            // The handshake has settled (once per mount): re-read
                            // the layout, since the mount's own load went out with
                            // no connection id to route it by.
                            let settled = Arc::clone(&mirror);
                            spawn_local(async move { settled.reload_tabs().await });
                        }
                        if flag.swap(false, Ordering::AcqRel) {
                            expiry.on_session_expired();
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

        // One read right away, so the first live frame carries the current prefs, then
        // keep polling.
        let now = Arc::clone(controller);
        spawn_local(async move { now.refresh_appearance().await });
        let poll = Arc::clone(controller);
        use_interval(APPEARANCE_POLL_INTERVAL, move || {
            let poll = Arc::clone(&poll);
            spawn_local(async move { poll.refresh_appearance().await });
        });

        // …and the layout poll beside it, for a mount that actually attached: a
        // fixture replay fronts no server whose tab world could move.
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

        Self {
            emulator,
            feed,
            input,
            selection,
            generation,
            reattach,
        }
    }

    /// Where this session's stream stands — [`LiveFeed::status`], forwarded.
    pub fn status(&self) -> Option<FeedStatus> {
        self.feed.status()
    }

    /// Re-open the stream: drop what is there and attach again at the grid's
    /// current size. A no-op on the demo replay.
    pub fn reattach(&self) {
        if let Some(reattach) = &self.reattach {
            reattach();
        }
    }

    /// The screen's *Retry*, offered on a disconnected link. Two cases, told apart by
    /// whether the feed's own pump is still there: a schedule that merely ran out is
    /// **parked**, and `reset_backoff` re-arms that very session; a session that ended
    /// for good (an expired credential) has no pump left, so only a fresh stream works.
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

    /// Tell the session the grid changed shape.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.feed.shared().borrow_mut().resize(cols, rows);
    }

    /// A cloneable handle onto this session's byte stream as the [`TerminalFeed`] trait
    /// object, for `presentation::pages`' wheel hook. The same live cell every call, so
    /// a re-attach that swaps [`feed`](Self::feed) is visible through an older handle.
    pub fn feed_handle(&self) -> term::SharedFeed {
        self.feed.shared()
    }
}

impl AttachSpec {
    /// Open a stream for this session at a `cols` x `rows` grid. `None` when nothing is
    /// connected: a connection torn down underneath a mounted screen is reachable, and
    /// must degrade (fixture replay, "nothing re-attached" on a retry) rather than panic.
    fn attach(&self, cols: u16, rows: u16) -> Option<GrpcTerminalFeed> {
        let client = self.connection.client()?;
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

    /// The re-attach closure [`LiveSession::reattach`] holds: replace the feed's
    /// contents in place, so the pump and the seams over the same cell keep working and
    /// the old stream is shut down by its own `Drop`.
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

/// Re-flagging the active tab in place, so the sheet's pills agree with the card.
trait ActiveTab {
    /// Mark `key`'s tab active and every other tab not — a no-op once the mount is
    /// gone, since this runs behind a tab switch's own round trip.
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
    // `#[cfg(test)]` exception: the one place terminal's `presentation` names a sibling
    // feature's `data`. `di.rs` stays the only *production* site where the two meet.
    use std::collections::VecDeque;

    use crate::features::sessions::data::repositories::MockSessionsRepository;
    use crate::features::settings::data::repositories::MockPreferencesRepository;
    use crate::features::settings::domain::entities::AppearancePrefs;
    use crate::features::terminal::data::feeds::grpc_feed::ResumeTarget;
    use crate::features::terminal::data::repositories::MockTerminalRepository;

    /// The test-wiring composition root: all three mocks behind their domain traits.
    fn controller(session_id: Option<&str>) -> TerminalController {
        TerminalController::new(
            Arc::new(MockTerminalRepository::seeded()),
            Arc::new(MockSessionsRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            session_id.map(str::to_string),
        )
    }

    /// Drive a controller call to completion on a current-thread runtime.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn a_load_lands_on_the_mocks_active_tab() {
        let c = controller(Some("zellij:dev"));
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

        let zellij = controller(Some("zellij:dev"));
        block_on(zellij.load());
        assert!(!zellij.has_spaces());
        assert!(zellij.spaces.get_untracked().is_empty());

        // A bare `/terminal` has no backend at all, so it has no spaces either.
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
            Some("ed@precision:~$ btop".to_string())
        );
        assert!(
            c.reveal_epoch.get_untracked() > before,
            "the replay token advances on every switch"
        );
        // …and the sheet's pills follow the card.
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
        // Spaces are herdr's, so every space test attaches to one.
        let c = controller(Some("herdr:work"));
        block_on(c.load());

        block_on(c.select_space("cupline".to_string()));

        let selected: Vec<String> = c
            .spaces
            .get_untracked()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.name)
            .collect();
        assert_eq!(selected, ["cupline"]);
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
            Some("zellij:dev".to_string()),
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
        let attached = controller(Some("zellij:dev"));
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
        // The demo row mounts the fixture replay on a screen that could attach
        // — the entry, not the screen, is what decides which feed it gets.
        let demo = controller(Some("zellij:dev"));
        demo.enter_live();
        assert!(demo.live.get_untracked());
        assert_eq!(demo.attached_session(), None);

        let attached = controller(Some("zellij:dev"));
        attached.attach_live();
        assert!(attached.live.get_untracked());
        assert_eq!(attached.attached_session().as_deref(), Some("zellij:dev"));
    }

    #[test]
    fn a_bare_terminal_route_has_no_session_to_attach_to() {
        let c = controller(None);
        c.attach_live();
        assert_eq!(c.attached_session(), None);
    }

    #[test]
    fn the_backend_discriminator_reads_the_sessions_own_id_prefix() {
        // Spaces, the resume hint a landed commit records and the grid's tap
        // hook all branch on this one answer.
        assert!(is_herdr("herdr:work"));
        assert!(!is_herdr("zellij:dev"));
        assert!(!is_herdr("dev"));

        let c = controller(Some("herdr:work"));
        assert!(
            c.is_herdr_session(),
            "which backend a session runs on is a fact about the session, not \
             about whether a stream is open onto it"
        );
        assert!(c.has_spaces());
        assert!(!controller(Some("zellij:dev")).is_herdr_session());
        assert!(!controller(None).is_herdr_session());
    }

    #[test]
    fn a_tab_switch_reports_whether_it_landed() {
        let c = controller(Some("zellij:dev"));
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
        let c = controller(Some("zellij:dev"));
        assert_eq!(
            c.link.get_untracked(),
            None,
            "a screen with no live session reports no link"
        );

        c.mirror_link(FeedStatus::Reconnecting, None);
        assert_eq!(c.link.get_untracked(), Some(FeedStatus::Reconnecting));

        c.mirror_link(FeedStatus::Connected, Some("conn-1".to_string()));
        assert_eq!(c.link.get_untracked(), Some(FeedStatus::Connected));
    }

    #[test]
    fn a_minted_connection_id_re_binds_the_repository_and_an_unchanged_one_does_not() {
        // The repository is the only observer of a re-bind, so a recording
        // stand-in is what makes "wrote once, not once per tick" assertable.
        let repository = Arc::new(RecordingRepository::default());
        let c = TerminalController::new(
            Arc::clone(&repository) as Arc<dyn TerminalRepository + Send + Sync>,
            Arc::new(MockSessionsRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            Some("zellij:dev".to_string()),
        );
        assert_eq!(
            repository.binds(),
            1,
            "construction binds the session, before any stream exists"
        );

        c.mirror_link(FeedStatus::Connected, Some("conn-1".to_string()));
        assert_eq!(repository.binds(), 2);
        c.mirror_link(FeedStatus::Connected, Some("conn-1".to_string()));
        c.mirror_link(FeedStatus::Reconnecting, Some("conn-1".to_string()));
        assert_eq!(repository.binds(), 2, "an unchanged id re-binds nothing");

        // A re-attach mints a fresh id, and every later request must route to
        // it rather than to the stream that is gone.
        c.mirror_link(FeedStatus::Connected, Some("conn-2".to_string()));
        assert_eq!(repository.binds(), 3);
        assert_eq!(repository.last_binding(), Some("conn-2".to_string()));
    }

    /// A [`TerminalRepository`] answering a two-tab, two-panes-apiece world that
    /// **records every call**, so a flow's RPC sequence is assertable, binds included.
    #[derive(Default)]
    struct RecordingRepository {
        calls: Mutex<Vec<String>>,
        last: Mutex<Option<String>>,
        /// The one verb that answers a failure instead of landing.
        refuse: Mutex<Option<String>>,
        /// How long each successive call parks before answering, oldest first — lets a
        /// test land an earlier layout fetch *after* a later one.
        delays: Mutex<VecDeque<Duration>>,
        /// How many [`list_tabs`](TerminalRepository::list_tabs) calls have been taken,
        /// stamped into every answered tab's `label` while
        /// [`stamp_lists`](Self::stamp_lists) is on. Off by default: a stamped answer
        /// differs on every call and would defeat the unchanged-answer tests.
        lists: AtomicU64,
        stamp: AtomicBool,
        /// Whether a create has landed — it then shows up in every later `list_tabs`.
        created: AtomicBool,
        /// The pane a landed [`focus_pane`](TerminalRepository::focus_pane) moved the
        /// focus onto, re-applied to every later `list_tabs` answer: a server hands a
        /// client back the focus that client itself asked for, which is what makes an
        /// optimistic badge survive the reload behind a commit.
        focused: Mutex<Option<(u32, bool)>>,
        /// Which space a landed [`switch_space`](TerminalRepository::switch_space) moved
        /// the selection onto, flagged in every later `list_spaces` answer the way a real
        /// server would. `None` is [`SEEDED_SPACE`], this world's own resting selection.
        selected: Mutex<Option<String>>,
        /// The tab every later `list_tabs` flags active; `None` keeps `alpha`.
        active: Mutex<Option<String>>,
    }

    /// The space [`RecordingRepository`] starts out selected on.
    const SEEDED_SPACE: &str = "ws-1";

    impl RecordingRepository {
        /// Every call seen, oldest first.
        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// Only the calls that *move* something — a load's reads are noise here.
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

        /// Make `verb` answer [`AppFailure::Internal`] from now on.
        fn refuse(&self, verb: &str) {
            *self.refuse.lock().unwrap_or_else(PoisonError::into_inner) = Some(verb.to_string());
        }

        /// Park the next calls for `delays`, oldest first — see that field.
        fn delay_calls(&self, delays: impl IntoIterator<Item = Duration>) {
            self.delays
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(delays);
        }

        /// Stamp every later `list_tabs` answer with its own call number.
        fn stamp_lists(&self) {
            self.stamp.store(true, Ordering::Relaxed);
        }

        /// Answer `focus` on every later `list_tabs`, whatever this client asked
        /// for — how a layout read reporting the *daemon's* own focus (what a
        /// fresh herdr attach lands on) is staged.
        fn report_focus(&self, focus: (u32, bool)) {
            *self.focused.lock().unwrap_or_else(PoisonError::into_inner) = Some(focus);
        }

        /// Flag `key` active on every later `list_tabs` — another client moving focus.
        fn report_active(&self, key: &str) {
            *self.active.lock().unwrap_or_else(PoisonError::into_inner) = Some(key.to_string());
        }

        /// Park this call for whatever [`delay_calls`](Self::delay_calls) queued.
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

        /// Record `call` and answer whatever the refusal table says.
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

    /// One recorded tab: `key`/`tab_id`, and two panes whose ids are
    /// `tab_id * 10` (focused) and one past it.
    fn recorded_tab(index: u32, key: &str, active: bool) -> TabEntry {
        let base = index * 10;
        let pane = |id: u32, focused: bool| PaneEntry {
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
            // Taken before the park below, so the answer carries the number of
            // the round trip that *started* first even when it lands last.
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
                // Only inside the tab carrying that pane: focus is flagged per
                // tab, so the tabs left behind keep their own.
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
    }

    /// A controller over a fresh [`RecordingRepository`], with the first load
    /// already landed — every sheet-flow test below starts here.
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

    /// The focused pane of the tab keyed `key`, as the sheet's badge reads it.
    fn focused_pane(controller: &TerminalController, key: &str) -> Option<u32> {
        controller
            .tabs
            .get_untracked()
            .into_iter()
            .find(|tab| tab.key == key)
            .and_then(|tab| tab.panes.into_iter().find(|pane| pane.focused))
            .map(|pane| pane.id)
    }

    /// [`recorded`], reloaded as the Tablet's docked rail.
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
        let (c, repository) = docked("zellij:dev");
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
        let (c, repository) = recorded("zellij:dev");
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
        let (c, repository) = recorded("zellij:dev");

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
        let (c, repository) = recorded("zellij:dev");

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
        for session in ["zellij:dev", "herdr:work"] {
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
        let (c, repository) = recorded("zellij:dev");
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
        let (c, repository) = recorded("zellij:dev");
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
        // The first commit's tab step parks, so the second tap arrives while it
        // is genuinely in flight — the rapid double-tap this drops.
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

        // …and the claim released with it, so the sheet still commits.
        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));
        assert_eq!(focused_pane(&c, "alpha"), Some(11));
    }

    // The tree sheet's own browse state: expansion, the browsed-space peek and its
    // stale guard, and the cross-space commit.

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
        // The first expand's peek parks; the second answers immediately, so the
        // expand that *started* first lands last.
        repository.delay_calls([Duration::from_millis(60), Duration::ZERO]);

        block_on(async {
            tokio::join!(
                c.expand_space("ws-2".to_string()),
                c.expand_space("cupline".to_string()),
            );
        });

        assert_eq!(
            c.expanded_space.get_untracked(),
            Some("cupline".to_string()),
            "the later expand stands"
        );
        assert_eq!(
            c.browsed_space_tabs.get_untracked().map(|(id, _)| id),
            Some("cupline".to_string()),
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

        block_on(c.expand_space("cupline".to_string()));

        assert_eq!(
            c.expanded_space.get_untracked(),
            Some("cupline".to_string())
        );
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
        let c = controller(Some("zellij:dev"));
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
        // Reopening the sheet now seeds the space it actually landed on…
        c.seed_expansion();
        assert_eq!(c.expanded_space.get_untracked(), Some("ws-2".to_string()));

        // …and a second commit inside it takes the same-space path, rather
        // than switching to a space it is already on.
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
        // What a dismissal running between the tap and the commit's first await
        // does: the tab world the tapped row was drawn from is gone.
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
        // The poll's round trip parks, so it lands *behind* the commit that
        // started after it — carrying the tab world of the space this
        // connection was still on when it asked.
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
        // The switch's own reload parks, so the commit arrives while it is
        // genuinely in flight.
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

        // …and the claim released with the switch, so the sheet still commits.
        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));
        assert_eq!(focused_pane(&c, "alpha"), Some(11));
    }

    // The live grid's content-tap hook (`presentation::pages`' own wiring).

    #[test]
    fn a_grid_tap_focus_never_switches_tabs_and_reloads_after() {
        let (c, repository) = recorded("zellij:dev");
        let after_load = repository.calls();

        assert!(block_on(c.focus_pane_from_tap(11, false)));

        let since_load: Vec<String> = repository
            .calls()
            .into_iter()
            .skip(after_load.len())
            .collect();
        // The optimistic move itself is pinned by the `commit_pane` tests
        // (shared code path); what a grid tap adds is the reload behind it.
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
        let (c, repository) = recorded("zellij:dev");
        repository.refuse("focus_pane");

        assert!(!block_on(c.focus_pane_from_tap(11, false)));

        assert_eq!(repository.actions(), ["focus_pane(11,false)"]);
        assert_eq!(
            focused_pane(&c, "alpha"),
            Some(10),
            "nothing moved, and nothing was reloaded either"
        );
    }

    // The live grid's wheel hook (`presentation::pages`' own wiring).

    #[test]
    fn a_wheel_scroll_fallback_calls_scroll_pane_and_ignores_its_answer() {
        let (c, repository) = recorded("zellij:dev");

        block_on(c.scroll_pane_from_wheel(21, false, ScrollStep::Up));

        assert_eq!(repository.actions(), ["scroll_pane(21,false,Up)"]);
    }

    #[test]
    fn a_refused_wheel_scroll_fallback_is_silently_dropped() {
        let (c, repository) = recorded("herdr:work");
        repository.refuse("scroll_pane");

        // Fire-and-forget: no panic, and no `Result` to inspect.
        block_on(c.scroll_pane_from_wheel(21, false, ScrollStep::Down));

        assert_eq!(repository.actions(), ["scroll_pane(21,false,Down)"]);
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

    // The reconnect resume hint: which paths write it, and which must not.

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
        let (c, _repository) = recorded("zellij:dev");

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

        // What a poll answers with after a reconnect the hint has not fixed yet:
        // the daemon's own focused pane, not this client's.
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

        // The mock's own recorded fixture never lists anything named this —
        // a non-match must not clear the server's own selection either.
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
        let (c, repository) = recorded("zellij:dev");

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
        let (c, repository) = recorded("zellij:dev");

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
        let (c, repository) = recorded("zellij:dev");
        repository.refuse("new_tab");
        let tabs = c.tabs.get_untracked();

        block_on(c.new_tab("build".to_string()));

        assert_eq!(repository.actions(), ["new_tab(build)"]);
        assert_eq!(c.tabs.get_untracked(), tabs, "no reload behind the refusal");
    }

    #[test]
    fn a_pane_tap_opens_a_tiled_pane_and_a_long_press_opens_a_floating_one() {
        let (c, repository) = recorded("zellij:dev");

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
        let (c, repository) = recorded("zellij:dev");
        repository.refuse("new_pane");
        let tabs = c.tabs.get_untracked();

        block_on(c.new_pane(false));

        assert_eq!(repository.actions(), ["new_pane(false)"]);
        assert_eq!(c.tabs.get_untracked(), tabs, "no reload behind the refusal");
    }

    // The tree's cross-space create rows: a create always lands where the
    // session is, so the row under a browsed space/tab moves there first.

    #[test]
    fn an_add_tab_row_under_a_browsed_space_switches_to_it_before_creating() {
        let (c, repository) = recorded("herdr:work");
        block_on(c.expand_space("ws-2".to_string()));
        let after_expand = repository.calls().len();

        // The wiring's own two steps, with the naming dialog in between.
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
        let (c, repository) = recorded("zellij:dev");
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
        // The winning row's own reload parks, so the second row is tapped while
        // its switch-then-create sequence is genuinely mid-flight.
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
        let (c, repository) = recorded("zellij:dev");
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
        let (c, repository) = recorded("zellij:dev");
        repository.refuse("rename_tab");
        let tabs = c.tabs.get_untracked();

        block_on(c.rename_tab(1, "renamed".to_string()));

        assert_eq!(repository.actions(), ["rename_tab(1,renamed)"]);
        assert_eq!(c.tabs.get_untracked(), tabs, "no reload behind the refusal");
    }

    #[test]
    fn renaming_a_pane_reloads_the_tab_list_too() {
        let (c, repository) = recorded("zellij:dev");

        block_on(c.rename_pane(10, false, "renamed".to_string()));

        assert_eq!(
            repository.actions(),
            ["rename_pane(10,false,renamed)"],
            "a pane's title lives inside TabEntry::panes, so the same reload carries it"
        );
    }

    #[test]
    fn a_refused_pane_rename_reloads_nothing() {
        let (c, repository) = recorded("zellij:dev");
        repository.refuse("rename_pane");
        let tabs = c.tabs.get_untracked();

        block_on(c.rename_pane(10, false, "renamed".to_string()));

        assert_eq!(repository.actions(), ["rename_pane(10,false,renamed)"]);
        assert_eq!(c.tabs.get_untracked(), tabs, "no reload behind the refusal");
    }

    // The layout-refresh mechanism: staleness guard, poll claim, create settle
    // and its coalescing, attach-settled edge.

    /// Serializes the tests installing a process-global
    /// [`set_create_settle_for_tests`] override across parallel test threads.
    static SETTLE_OVERRIDE: Mutex<()> = Mutex::new(());

    /// An installed create-settle override, restored to the shipped
    /// [`CREATE_SETTLE`] when it drops.
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

    /// Every published tab's label, in order — the round-trip stamp
    /// [`RecordingRepository::stamp_lists`] writes.
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
        let (c, repository) = recorded("zellij:dev");
        repository.stamp_lists();
        // The first reload parks and the second answers immediately, so the
        // round trip that *started* first lands last.
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
        // A herdr session, so load() reads the space list at all.
        let repository = Arc::new(RecordingRepository::default());
        let c = TerminalController::new(
            Arc::clone(&repository) as Arc<dyn TerminalRepository + Send + Sync>,
            Arc::new(MockSessionsRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            Some("herdr:work".to_string()),
        );
        // load()'s first RPC (list_spaces) parks; reload_tabs()'s list_tabs
        // lands immediately, advancing the shared generation past load()'s.
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
        let c = controller(Some("zellij:dev"));
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
        let (c, _repository) = recorded("zellij:dev");
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
        let (c, _repository) = recorded("zellij:dev");

        // A snapshot that landed empty (what a load racing the attach
        // handshake leaves) recovers on the next reload.
        c.active_tab.set(String::new());
        block_on(c.reload_tabs());
        assert_eq!(c.active_tab.get_untracked(), "alpha");
    }

    #[test]
    fn a_landed_create_waits_out_the_settle_before_reading_back_what_it_made() {
        /// Unmistakable against instant mock calls, short enough not to pad.
        const SETTLE: Duration = Duration::from_millis(80);
        let _override = SettleOverride::install(SETTLE);
        let (c, repository) = recorded("zellij:dev");

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
        let (c, repository) = recorded("zellij:dev");
        // The first create parks, so the second press arrives while it is
        // genuinely in flight — the rapid double-tap this coalesces.
        repository.delay_calls([Duration::from_millis(60)]);

        block_on(async {
            tokio::join!(c.new_tab(String::new()), c.new_tab(String::new()));
        });

        assert_eq!(
            repository.actions(),
            ["new_tab()"],
            "a repeated press is dropped, never queued behind the first"
        );

        // …and the claim released with it, so the affordance still works.
        block_on(c.new_tab(String::new()));
        assert_eq!(repository.actions(), ["new_tab()", "new_tab()"]);
    }

    #[test]
    fn a_refused_create_re_arms_the_affordance_it_claimed() {
        let _override = SettleOverride::install(Duration::ZERO);
        let (c, repository) = recorded("zellij:dev");
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
        let c = controller(Some("zellij:dev"));
        assert!(
            !c.mirror_link(FeedStatus::Idle, None),
            "nothing has connected yet"
        );
        assert!(!c.mirror_link(FeedStatus::Reconnecting, None));

        assert!(
            c.mirror_link(FeedStatus::Connected, Some("conn-1".to_string())),
            "the stream minted an id: the layout read that raced it needs \
             doing again"
        );
        assert!(
            !c.mirror_link(FeedStatus::Connected, Some("conn-1".to_string())),
            "…and it is said once"
        );
        assert!(
            !c.mirror_link(FeedStatus::Connected, Some("conn-2".to_string())),
            "a re-attach re-binds the repository but does not re-settle"
        );
    }

    #[test]
    fn a_connected_status_settles_the_attach_even_with_no_id_to_show_for_it() {
        let c = controller(Some("zellij:dev"));
        assert!(c.mirror_link(FeedStatus::Connected, None));
        assert!(!c.mirror_link(FeedStatus::Connected, None));
    }

    #[test]
    fn the_live_pane_seeds_the_shipped_appearance_defaults() {
        // Before the first `refresh_appearance` lands, the first live frame
        // must show the shipped default rather than a blank/wrong shape.
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

    /// A controller wired over the same [`MockPreferencesRepository`] `prefs`
    /// names, so a test can `save_appearance` straight into the store
    /// [`TerminalController::refresh_appearance`] reads.
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
        // A family the store persists happily but this device has no face for
        // — the unknown-family grace `fonts::resolve_terminal_family` owns.
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

        // Nothing in the store moved, so the read-compare-then-write gate must
        // write neither the signal nor the counter on this identical tick.
        block_on(c.refresh_appearance());

        assert_eq!(c.font_family.get_untracked(), resolved);
        assert_eq!(
            c.appearance_writes(),
            after_first,
            "an idle poll over an unchanged font_family pref must write nothing"
        );
    }
}
