//! [`TerminalController`] — the `/terminal` screen's view model.
//!
//! Mirrors the shape `docs/ARCHITECTURE.md`'s **Building a Screen** section
//! mandates and `features::sessions`' own controller establishes: a
//! [`ControllerCore`] embedded by composition, the feature's four use cases
//! constructed over two injected repositories, a third injected repository
//! (`settings`' own preferences store, read directly by the appearance poll
//! rather than through a use case), and the signals the screen renders. Every
//! injected repository is resolved exactly **once**, by `pages.rs`'s
//! `use_controller` factory — never re-resolved from inside a method this
//! controller runs later (see [`refresh_appearance`](Self::refresh_appearance)'s
//! own note). Hosted by the screen through `clean_signals_frust::use_controller`,
//! so it is created on route mount and disposed on unmount.
//!
//! # Signals vs. plain state, and where the *animations* live
//!
//! Everything below is observed state a rebuild depends on: the tab world, the
//! active tab, its lines, whether the screen is fullscreen. Writing one wakes
//! the screen's rebuild, which is exactly what each is for.
//!
//! What is deliberately **not** here is per-frame animation progress. The
//! card's staggered line reveal, its blinking caret and the fullscreen
//! clearance/padding/radius transition are all advanced from `Widget::paint`
//! against the shell's frame clock (`crate::features::terminal::presentation::chrome`),
//! because a signal written once per frame would wake a whole-app rebuild per
//! frame — the one thing `docs/ARCHITECTURE.md`'s rebuild model rules out.
//! The controller instead publishes the *cue*: [`reveal_epoch`](Self::reveal_epoch)
//! is a monotonic replay token, bumped whenever the reveal should start over,
//! and the body widget restarts its own driver when the number it was handed
//! changes. One rebuild per replay; zero per frame.
//!
//! # Fullscreen is controller state, not widget state
//!
//! [`fullscreen`](Self::fullscreen) drives three separate things — the card's
//! own geometry, the expand button's on-state and icon, and the *shell bar's*
//! hidden flag (published through `crate::routes`) — so it cannot live inside
//! any one widget. It also has to survive the sheet/menu pages pushed over the
//! screen, which is another way of saying it belongs to the screen's view
//! model.
//!
//! # The link status is polled, never pushed
//!
//! A feed writes no signal and touches no reactive state at all — its own
//! module docs make that a contract, because its pump runs off the UI thread.
//! So the connection status a live session's banner renders is **mirrored**
//! here instead: [`LiveSession`] polls the feed on every wake of the UI-thread
//! repaint pump and calls [`TerminalController::mirror_link`], which is the one
//! place that fact becomes a signal. Read-compare-then-write, like
//! [`refresh_appearance`](TerminalController::refresh_appearance): a status
//! that has not moved wakes no rebuild, so an idle session still costs zero
//! frames.
//!
//! The same tick carries two more things the feed can only report by being
//! asked: the connection id its stream minted (re-[`bind`](TerminalRepository::bind)ed
//! onto the repository whenever it changes, so every later request routes to
//! *this* client's view) and the auth-expired edge, which the feed's own
//! callback can only raise as a flag because it fires on the background
//! runtime, where writing a signal is forbidden.
//!
//! # Two switches call the repository directly
//!
//! [`select_space`](TerminalController::select_space) and
//! [`switch_tab`](TerminalController::switch_tab) each tell the backend where
//! the session is now looking, and neither has anything to compose: they are
//! one repository call apiece, so they are made directly and their failures
//! are emitted on the very same [`ControllerCore`] sink `run` funnels into.
//! The *reads* around them still go through their use cases, so the screen's
//! one loading/activity story is unchanged.
//!
//! # Creating something settles by reloading, never by a dialog answer
//!
//! [`create_space`](TerminalController::create_space),
//! [`new_tab`](TerminalController::new_tab) and
//! [`new_pane`](TerminalController::new_pane) each mutate first (their own
//! [`TerminalRepository`] verb, called directly — the same shape the two
//! switches above use) and then settle. [`new_tab`](TerminalController::new_tab)/
//! [`new_pane`](TerminalController::new_pane) settle with a full
//! [`load`](TerminalController::load), because the tab world itself moved
//! and the server focuses whatever it just opened — `load`'s own
//! active/browsed re-seed is exactly the "the sheet follows the server"
//! behaviour those two want, for free.
//! [`create_space`](TerminalController::create_space) is the exception:
//! creating a space touches no tab, so it re-lists **only** the spaces
//! (again a direct repository call) rather than replaying the reveal and
//! reseeding the browse for nothing — and since the wire's ack carries no
//! id, it flags the freshly listed space whose name matches what was typed
//! selected in the sheet's own state, a client-side courtesy pick rather
//! than a second RPC.
//!
//! # Browsing the sheet is local; only a pane tap commits
//!
//! The "panes & tabs" sheet is a **browse-then-commit** surface, so it has two
//! notions of "the tab": [`active_tab`](TerminalController::active_tab) is the
//! tab the *session* is on (what the card shows), and
//! [`browsed_tab`](TerminalController::browsed_tab) is the tab the sheet is
//! *looking at*. Tapping a tab pill moves the second one and nothing else —
//! no RPC, no reattach, no card change — because
//! [`TabEntry::panes`](crate::features::terminal::domain::entities::TabEntry::panes)
//! already carries every tab's panes from the one load. The two are reconciled
//! by [`commit_pane`](TerminalController::commit_pane), the sheet's only
//! committing path: it moves the session (a tab step when the two differ, then
//! the pane focus itself) and only then may the sheet close.
//!
//! # One backend discriminator, two consequences
//!
//! A session id is `<backend>:<name>`, and this screen reads exactly one fact
//! off it ([`is_herdr`]): whether the session runs on herdr. Two behaviours
//! follow, and neither re-parses the id — spaces exist only on herdr
//! ([`has_spaces`](TerminalController::has_spaces), which is also what keeps a
//! zellij load from round-tripping for a guaranteed-empty list), and only
//! herdr's stream has to be re-opened for a focus move to become visible
//! ([`reattaches_on_switch`](TerminalController::reattaches_on_switch)).

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clean_signals::{ControllerCore, RunOptions};
use clean_signals_frust::use_interval;
use frust::{GetUntracked, RwSignal, Set, spawn_local};

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::terminal::data::feeds::demo_feed::DemoFeed;
use crate::features::terminal::data::feeds::grpc_feed::{FeedStatus, GrpcTerminalFeed};
use crate::features::terminal::domain::entities::{
    PaneEntry, RenameRequest, SettingsCursorStyle, Space, TabEntry, TermLine, Workspace,
};
use crate::features::terminal::domain::feed::TerminalFeed;
use crate::features::terminal::domain::repositories::{
    PreferencesRepository, SessionsRepository, TerminalRepository,
};
use crate::features::terminal::domain::use_cases::{
    DetachSession, LoadWorkspace, RenameAttachedSession, SwitchTab, WorkspaceQuery,
};

use super::term;

/// The `/terminal` screen's view model. See the [module docs](self).
pub struct TerminalController {
    core: ControllerCore<AppFailure>,
    load: LoadWorkspace,
    switch: SwitchTab,
    rename: RenameAttachedSession,
    detach: DetachSession,
    /// This feature's own repository, kept beside the use cases built over it
    /// so the two switches can call it directly — see the [module docs](self)'
    /// "Two switches call the repository directly" — and so
    /// [`mirror_link`](Self::mirror_link) can re-bind it to a freshly minted
    /// connection id.
    terminal: Arc<dyn TerminalRepository + Send + Sync>,
    /// The attached session's id (`/terminal?session=<id>`), or `None` for a
    /// bare `/terminal`. Plain data: it is fixed for this mount.
    session_id: Option<String>,
    /// The sheet's `spaces` pill row.
    pub spaces: RwSignal<Vec<Space>>,
    /// The sheet's `tabs` pill row.
    pub tabs: RwSignal<Vec<TabEntry>>,
    /// The key of the tab the card is showing — the tab the *session* is on.
    pub active_tab: RwSignal<String>,
    /// The key of the tab the sheet is *looking at* — see the [module
    /// docs](self)' "Browsing the sheet is local". Seeded from
    /// [`active_tab`](Self::active_tab) on every load and every sheet open,
    /// and moved by [`browse_tab`](Self::browse_tab) alone.
    pub browsed_tab: RwSignal<String>,
    /// The active tab's lines — what the card's body prints.
    pub lines: RwSignal<Vec<TermLine>>,
    /// The attached session's current name, for the rename dialog's seed.
    pub session_name: RwSignal<Option<String>>,
    /// Whether the first load has landed (the card renders an empty body
    /// rather than a wrong one until it has).
    pub loaded: RwSignal<bool>,
    /// Whether the card is fullscreen — see the [module docs](self).
    pub fullscreen: RwSignal<bool>,
    /// The reveal replay token — see the [module docs](self).
    pub reveal_epoch: RwSignal<u64>,
    /// Mints [`reveal_epoch`](Self::reveal_epoch) values. An atomic rather
    /// than a read-modify-write on the signal itself, so a bump made from a
    /// press handler (no reactive owner on desktop) never reads a signal it
    /// is about to write.
    next_epoch: AtomicU64,
    /// Whether the card shows the live pane instead of the mock transcript —
    /// see the [module docs](self)'s "Pane-content switch" note and
    /// [`LiveSession`]. Entered once per mount, either by
    /// [`attach_live`](Self::attach_live) (a real session with a live client,
    /// decided at mount) or by [`enter_live`](Self::enter_live) (the
    /// checked-in fixture replay); there is no path back to the mock
    /// transcript within the same mount.
    pub live: RwSignal<bool>,
    /// Which of the two the live pane is: `true` once
    /// [`attach_live`](Self::attach_live) has claimed it for a real `muxrd`
    /// session, `false` for the fixture replay.
    ///
    /// A plain flag rather than a signal: it is decided once, before the first
    /// build that observes [`live`](Self::live), and nothing rendered branches
    /// on it — [`LiveSession::enter`] reads it to pick a feed, and that is all.
    live_attached: AtomicBool,
    /// Where a live `muxrd` session's stream currently stands, mirrored off
    /// the feed on the UI thread — see the [module docs](self)' "The link
    /// status is polled". `None` while the screen has no such session (the
    /// mock transcript, and the demo replay, both of which have no link to
    /// report).
    pub link: RwSignal<Option<FeedStatus>>,
    /// The connection id last handed to the repository, so a poll tick that
    /// sees an unchanged one re-binds nothing (a `Mutex`, not a `Cell`,
    /// because this type is `Send + Sync` under `use_controller`'s bounds).
    bound_connection_id: Mutex<String>,
    /// The live pane's cursor shape, mapped from `settings`' own
    /// `AppearancePrefs::cursor_style` (see [`refresh_appearance`](Self::refresh_appearance)).
    /// Seeded with the shipped default (`Preferences::defaults()`'s own
    /// `CursorStyle::Block`) so the very first live frame — before that
    /// first read lands — never shows the wrong shape.
    pub cursor_style: RwSignal<term::CursorStyle>,
    /// The live pane's font size in px, mapped from `AppearancePrefs::font_size_px`.
    /// Seeded with the shipped default (`12px`) for the same reason as
    /// [`cursor_style`](Self::cursor_style).
    pub font_size: RwSignal<f32>,
    /// The device-local preferences repository — resolved **once** by
    /// `pages.rs`'s `use_controller` factory (alongside the terminal/sessions
    /// repositories above) and stored here, per `docs/ARCHITECTURE.md`'s
    /// Building a Screen rule that an app-scoped dependency is resolved once
    /// at construction, never re-resolved from inside a method that runs
    /// later (see [`refresh_appearance`](Self::refresh_appearance), which
    /// used to call `settings::di::expect_preferences_repository()` — a
    /// `use_context` lookup — on every poll tick).
    preferences: Arc<dyn PreferencesRepository + Send + Sync>,
    /// Test/diagnostic-only: how many times [`refresh_appearance`](Self::refresh_appearance)
    /// has actually written [`cursor_style`](Self::cursor_style)/
    /// [`font_size`](Self::font_size) since construction — never read by
    /// production code. Lets a headless test pin that an unchanged-prefs poll
    /// writes nothing (see that method's own doc on the
    /// read-compare-then-write gate this counts), without inventing a
    /// heavier notify/version probe. Exposed read-only via
    /// [`appearance_writes`](Self::appearance_writes), the same plain
    /// `debug_`-style accessor `pages.rs` already uses
    /// (`debug_controller`/`debug_live_probe`) for reaching real
    /// composition-root state from `tests/*.rs` — those integration suites
    /// link this crate without `cfg(test)`, so a literal `#[cfg(test)]` item
    /// would be invisible to them.
    appearance_writes: AtomicU64,
}

/// The shipped default font size, px — settings' own `Preferences::defaults()`'s
/// `AppearancePrefs::font_size_px`, transcribed rather than reached for (this
/// feature's `presentation` maps `settings`' values; it does not construct
/// them — see [`TerminalController::refresh_appearance`]'s own note on this
/// sibling-`domain` widening).
const DEFAULT_LIVE_FONT_SIZE: f32 = 12.0;

/// How often the live pane re-reads the terminal-appearance prefs while
/// mounted. [`PreferencesRepository`] has no subscribe/push seam (its own
/// module docs), so a `clean_signals_frust::use_interval` poll is the
/// established mechanism this app already uses for a periodic re-read.
///
/// **3s — matching `sessions`' own mock-poll cadence**
/// (`sessions::presentation::controllers::POLL_PERIOD_SECS`, transcribed
/// rather than imported: a feature never imports a sibling's `presentation`,
/// same rule `pages.rs`'s `SESSION_QUERY_KEY` already follows). The original
/// 250ms (12× that cadence) bought nothing: a `/settings` edit is only ever
/// made from a *different*, pushed-on-top screen — this app has no way to
/// view `/terminal` and `/settings` at once — so it can only ever land while
/// this pane sits retained underneath, and the user sees it within one beat
/// of popping back either way. [`refresh_appearance`](TerminalController::refresh_appearance)
/// also now reads-compares-writes (below), so even this coarser cadence's
/// idle ticks wake no signal, and no rebuild, once prefs settle.
const APPEARANCE_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Map `settings`' own `CursorStyle` (reached via
/// [`entities::SettingsCursorStyle`](crate::features::terminal::domain::entities::SettingsCursorStyle),
/// never `settings::domain` directly) onto this feature's `term::CursorStyle`.
///
/// A narrow, documented sibling-`domain` widening — the same shape
/// `docs/ARCHITECTURE.md` already records for `terminal`'s reuse of
/// `sessions`' `RenameRequest`/`SessionsRepository`, and the third use of
/// that pattern this app records: a cursor shape has exactly one owner (the
/// settings screen's own picker and its store), so this screen maps rather
/// than re-minting a second choice set. Variants line up 1:1
/// (`term::CursorStyle`'s own doc), so the match is exhaustive and a variant
/// added to one enum without the other fails to compile rather than silently
/// falling back.
fn map_cursor_style(style: SettingsCursorStyle) -> term::CursorStyle {
    match style {
        SettingsCursorStyle::Block => term::CursorStyle::Block,
        SettingsCursorStyle::Underline => term::CursorStyle::Underline,
        SettingsCursorStyle::Bar => term::CursorStyle::Bar,
    }
}

impl TerminalController {
    /// A fresh controller over the injected repositories (the composition root
    /// built each once — see [`crate::features::terminal::di`],
    /// [`crate::features::sessions::di`] and
    /// [`crate::features::settings::di`]), attached to `session_id`.
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
            terminal,
            session_id,
            spaces: RwSignal::new(Vec::new()),
            tabs: RwSignal::new(Vec::new()),
            active_tab: RwSignal::new(String::new()),
            browsed_tab: RwSignal::new(String::new()),
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
            preferences,
            appearance_writes: AtomicU64::new(0),
        }
    }

    /// This controller's [`ControllerCore`] — the failure sink the screen
    /// subscribes its toast bridge to (`use_failure_listener`). A named
    /// accessor rather than the [`AsRef`] impl below because
    /// `Arc<TerminalController>` has its own inherent `as_ref`, which would
    /// make the bare call ambiguous at every site.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// The attached session's id, if this screen was reached with one.
    pub fn session_id(&self) -> Option<String> {
        self.session_id.clone()
    }

    /// Load the whole screen (spaces, tabs, the active tab's lines, the
    /// attached session's name) and play the reveal.
    ///
    /// The space list is only read on a backend that has one — see
    /// [`has_spaces`](Self::has_spaces). Landing also re-seeds
    /// [`browsed_tab`](Self::browsed_tab): a load is the one moment the sheet
    /// has nothing of its own to preserve, and a browse pointed at a tab this
    /// load no longer carries would list no panes at all.
    pub async fn load(&self) {
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
        let active = tabs
            .iter()
            .find(|tab| tab.active)
            .or_else(|| tabs.first())
            .map(|tab| tab.key.clone())
            .unwrap_or_default();
        self.spaces.set(spaces);
        self.tabs.set(tabs);
        self.active_tab.set(active.clone());
        self.browsed_tab.set(active);
        self.lines.set(lines);
        self.session_name.set(session_name);
        self.loaded.set(true);
        self.replay_reveal();
    }

    /// Point the sheet at the tab keyed `key` — see the [module docs](self)'
    /// "Browsing the sheet is local".
    ///
    /// Deliberately **synchronous and repository-free**: every tab's panes
    /// arrived with the load, so browsing one is a signal write and nothing
    /// else. Nothing about the session moves, so the card, the stream and the
    /// server's own focus all stay exactly where they were.
    pub fn browse_tab(&self, key: String) {
        self.browsed_tab.set(key);
    }

    /// Point the sheet back at the tab the session is on — what opening the
    /// sheet does, so a browse never survives a dismissal.
    pub fn begin_browsing(&self) {
        self.browsed_tab.set(self.active_tab.get_untracked());
    }

    /// Move the session to the tab keyed `key`: the session is told to focus
    /// that tab, its lines replace the body's, and the reveal replays from the
    /// top (the mock's own behaviour — `renderTab` ends in `playReveal`).
    ///
    /// The **tab step of a cross-tab pane commit**
    /// ([`commit_pane`](Self::commit_pane)), which is its one caller: a tab
    /// pill no longer navigates anything on its own (see the [module
    /// docs](self)' "Browsing the sheet is local"). Answers whether the move
    /// landed; whether the attached *stream* must be re-opened on top of it is
    /// [`reattaches_on_switch`](Self::reattaches_on_switch)'s answer, since the
    /// caller owns the feed and this does not.
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
        self.browsed_tab.set(key.clone());
        self.tabs.update_active(&key);
        self.lines.set(lines);
        self.replay_reveal();
        true
    }

    /// Commit the sheet's browse: focus the pane `pane_id` of the tab keyed
    /// `tab_key` (`is_plugin` picking the wire's id namespace, as everywhere
    /// else a pane is addressed).
    ///
    /// The sheet's **one** committing path, in the order a backend needs it:
    ///
    /// 1. a tab move first, when the browsed tab is not the one the session is
    ///    on — a pane can only be focused inside the focused tab;
    /// 2. then the pane focus itself, **except on herdr**, where focus *is* the
    ///    attach (its relay carries only the focused pane, so the re-attach the
    ///    caller performs is what moves it) and the RPC would be a no-op;
    /// 3. then the focused flag moves in this screen's own tab state, so the
    ///    sheet's badge is already right the instant it is reopened rather than
    ///    waiting for the next load.
    ///
    /// Answers whether the commit landed. A step the backend refused stops the
    /// sequence, emits on the usual failure sink, and writes **no** optimistic
    /// state — so a caller keeps the sheet open on `false`.
    pub async fn commit_pane(&self, tab_key: String, pane_id: u32, is_plugin: bool) -> bool {
        if tab_key != self.active_tab.get_untracked() && !self.switch_tab(tab_key.clone()).await {
            return false;
        }
        if !self.is_herdr_session()
            && !self
                .tell(self.terminal.focus_pane(pane_id, is_plugin))
                .await
        {
            return false;
        }
        self.focus_pane_locally(&tab_key, pane_id, is_plugin);
        true
    }

    /// Move the focused flag onto the pane `pane_id` of the tab keyed
    /// `tab_key`, in this screen's own state — the optimistic half of
    /// [`commit_pane`](Self::commit_pane).
    ///
    /// One focused pane per tab, whatever layer it is on: the wire flags focus
    /// per layer (a tiled *and* a floating pane can both carry it), but the
    /// sheet shows one badge and the commit just moved the user to exactly one
    /// pane, so the flag is cleared across the tab and set on the tapped pane.
    /// The next load replaces this with the backend's own answer.
    fn focus_pane_locally(&self, tab_key: &str, pane_id: u32, is_plugin: bool) {
        let tabs = self
            .tabs
            .get_untracked()
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

    /// Make the space with [`Space::id`] `id` the active one and reload the
    /// tab world behind it.
    ///
    /// A **view** switch, not a mutation (see [`TerminalRepository::switch_space`]):
    /// which space this connection is looking at is per-connection state, so
    /// this stays available to a read-only credential.
    ///
    /// The reload is the point: a space *is* a different set of tabs, so
    /// switching one without re-reading them left the sheet listing the tabs of
    /// the space it had just left. [`load`](Self::load) is that read — it also
    /// re-seeds the space selection from the backend's own answer rather than
    /// from a local guess, and points the sheet's browse at the new space's
    /// active tab.
    pub async fn select_space(&self, id: String) {
        if !self.tell(self.terminal.switch_space(id)).await {
            return;
        }
        self.load().await;
    }

    /// Create a new space named `label` (already trimmed by the dialog that
    /// collected it — `pages::Actions::open_create_space_dialog`) and pull
    /// the sheet's own space list up to date.
    ///
    /// The wire's ack carries no id (original-app parity), so nothing here
    /// can address the space it just created the way
    /// [`select_space`](Self::select_space) addresses one it already knows —
    /// this never calls [`TerminalRepository::switch_space`]. What it does
    /// instead, once the create itself lands, is re-list the spaces
    /// ([`TerminalRepository::list_spaces`], called directly for the same
    /// reason [`select_space`](Self::select_space)/[`switch_tab`](Self::switch_tab)
    /// already do — see the [module docs](self)) and, on a name match, flag
    /// that one selected in the sheet's **own** state: a local courtesy
    /// pick, not a second round trip. A non-match (the backend renamed it,
    /// or named an empty `label` itself) is silently fine — whatever the
    /// server actually answered stays exactly as reloaded.
    ///
    /// Deliberately **not** a full [`load`](Self::load): creating a space
    /// touches nothing about the tab world, so re-reading it too would
    /// replay the reveal and reseed the browse for no reason. The server
    /// validates `label`'s charset/length; a rejection reaches
    /// [`tell`](Self::tell)'s own failure sink like every other refused
    /// verb, and this writes nothing when that happens.
    pub async fn create_space(&self, label: String) {
        if !self.tell(self.terminal.create_space(label.clone())).await {
            return;
        }
        match self.terminal.list_spaces().await {
            Ok(spaces) => self.spaces.set(select_by_label(spaces, &label)),
            Err(failure) => self.core.failures().emit(&failure),
        }
    }

    /// Open a new, unnamed tab (the wire names it) and reload the workspace
    /// so it appears — the `+` tab pill's press (`pages::Actions::new_tab`),
    /// with no dialog in front of it (original-app parity).
    ///
    /// A full [`load`](Self::load), unlike [`create_space`](Self::create_space):
    /// the tab world itself changed, and the server focuses the tab it just
    /// opened, so `load`'s own active/browsed re-seed is exactly what puts
    /// the sheet's browse on it — nothing else has to chase that fact down.
    pub async fn new_tab(&self) {
        if self.tell(self.terminal.new_tab()).await {
            self.load().await;
        }
    }

    /// Open a new pane in the tab the session is on — tiled (`floating =
    /// false`, a tap on the new-pane row) or floating (`floating = true`, a
    /// long-press on it). A backend that ignores the flag (herdr does) still
    /// receives it, so there is no client-side gating to add. Reloads the
    /// workspace on landing, the same shape as [`new_tab`](Self::new_tab),
    /// so the sheet's pane list picks it up.
    pub async fn new_pane(&self, floating: bool) {
        if self.tell(self.terminal.new_pane(floating)).await {
            self.load().await;
        }
    }

    /// Rename the space with [`Space::id`] `space_id` to `label` (already
    /// trimmed by the dialog that collected it —
    /// `pages::Actions::open_rename_space_dialog`) and pull the sheet's own
    /// space list up to date.
    ///
    /// Deliberately **not** a full [`load`](Self::load), the identical
    /// reasoning [`create_space`](Self::create_space) already documents:
    /// renaming a space touches no tab, so replaying the reveal and reseeding
    /// the browse would buy nothing. No courtesy re-select is needed here the
    /// way `create_space` needs one either — [`Space::id`] never moves on a
    /// rename, so the freshly listed row the server answers with is already
    /// the one that matters, selected or not.
    pub async fn rename_space(&self, space_id: String, label: String) {
        if !self.tell(self.terminal.rename_space(space_id, label)).await {
            return;
        }
        match self.terminal.list_spaces().await {
            Ok(spaces) => self.spaces.set(spaces),
            Err(failure) => self.core.failures().emit(&failure),
        }
    }

    /// Rename the tab with [`TabEntry::tab_id`] `tab_id` to `name` (already
    /// trimmed by the dialog — `pages::Actions::open_rename_tab_dialog`), then
    /// pull the sheet's own tab list up to date via [`reload_tabs`](Self::reload_tabs).
    ///
    /// Deliberately **not** a full [`load`](Self::load) either, unlike
    /// [`new_tab`](Self::new_tab)/[`new_pane`](Self::new_pane): a rename
    /// creates nothing for the server to refocus, so
    /// [`active_tab`](Self::active_tab)/[`browsed_tab`](Self::browsed_tab)/
    /// [`lines`](Self::lines) stay exactly where they were — only the label
    /// (and the panes list it rides along with) has to catch up.
    pub async fn rename_tab(&self, tab_id: u64, name: String) {
        if self.tell(self.terminal.rename_tab(tab_id, name)).await {
            self.reload_tabs().await;
        }
    }

    /// Rename the pane `pane_id` to `name` (`is_plugin` picking the wire's id
    /// namespace, as everywhere else a pane is addressed; already trimmed by
    /// the dialog — `pages::Actions::open_rename_pane_dialog`), then pull the
    /// tab list up to date the identical way [`rename_tab`](Self::rename_tab)
    /// does — a pane's title lives inside [`TabEntry::panes`], so the same
    /// reload carries it.
    pub async fn rename_pane(&self, pane_id: u32, is_plugin: bool, name: String) {
        if self
            .tell(self.terminal.rename_pane(pane_id, is_plugin, name))
            .await
        {
            self.reload_tabs().await;
        }
    }

    /// Re-list the tabs and publish them — the reload
    /// [`rename_tab`](Self::rename_tab)/[`rename_pane`](Self::rename_pane)
    /// share: neither creates anything the server might refocus, so re-reading
    /// [`tabs`](Self::tabs) alone (no [`load`](Self::load), no reveal replay)
    /// is enough to catch a changed label up.
    async fn reload_tabs(&self) {
        match self.terminal.list_tabs().await {
            Ok(tabs) => self.tabs.set(tabs),
            Err(failure) => self.core.failures().emit(&failure),
        }
    }

    /// Whether this session's backend has **spaces** at all — the one fact the
    /// sheet gates its whole spaces section on.
    ///
    /// Only herdr does: zellij's own `GetSpaces` answers an always-empty list
    /// (`proto/muxr.proto`), so a zellij sheet draws no spaces section and a
    /// zellij load skips that round trip entirely. Read off the session id's
    /// `<backend>:<name>` convention through the single discriminator this
    /// screen keeps ([`is_herdr`]) — see the [module docs](self)' "One backend
    /// discriminator".
    pub fn has_spaces(&self) -> bool {
        self.is_herdr_session()
    }

    /// Whether the attached session runs on herdr — the discriminator itself,
    /// spelled once so no call site re-parses the id.
    ///
    /// Answers off [`session_id`](Self::session_id) rather than
    /// [`attached_session`](Self::attached_session): which backend a session
    /// runs on is a fact about the session, not about whether a stream happens
    /// to be open onto it (which is what
    /// [`reattaches_on_switch`](Self::reattaches_on_switch) additionally
    /// needs).
    fn is_herdr_session(&self) -> bool {
        self.session_id.as_deref().is_some_and(is_herdr)
    }

    /// The backend id of the tab keyed `key`, if the sheet is still showing
    /// it.
    fn tab_id_for(&self, key: &str) -> Option<u64> {
        self.tabs
            .get_untracked()
            .into_iter()
            .find(|tab| tab.key == key)
            .map(|tab| tab.tab_id)
    }

    /// Drive one repository call that has nothing to hand back, reporting
    /// whether it landed and emitting whatever it did not on the same failure
    /// sink `ControllerCore::run` uses — see the [module docs](self)' "Two
    /// switches call the repository directly".
    async fn tell(&self, work: impl Future<Output = Result<(), AppFailure>>) -> bool {
        match work.await {
            Ok(()) => true,
            Err(failure) => {
                self.core.failures().emit(&failure);
                false
            }
        }
    }

    /// Rename the attached session to `name` (already trimmed by the dialog),
    /// writing through the **sessions** repository so the list reflects it on
    /// the way back. Answers the stored name it landed on, for the toast.
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

    /// Detach from the attached session. Answers whether the call landed, so
    /// the caller only pops (and toasts) on success.
    pub async fn detach(&self) -> bool {
        let Some(id) = self.session_id.clone() else {
            return false;
        };
        self.core
            .run(&self.detach, id, RunOptions::default())
            .await
            .is_ok()
    }

    /// Flip fullscreen and answer the new state — the expand button's whole
    /// behaviour (the caller publishes it to the shell bar).
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

    /// Bump the reveal replay token — see the [module docs](self).
    fn replay_reveal(&self) {
        self.reveal_epoch
            .set(self.next_epoch.fetch_add(1, Ordering::Relaxed));
    }

    /// Enter the live pane over the **checked-in fixture replay**.
    ///
    /// No production caller since the overflow menu's *Demo session* row was
    /// withdrawn: this is now the seam `tests/terminal_live.rs` drives the real
    /// grid through, reaching the mounted screen's controller with
    /// `pages::debug_controller` (that suite's own module docs record the
    /// arrangement). Everything downstream is unchanged, which is the point —
    /// the tests exercise the composition a device runs.
    ///
    /// Idempotent from this signal's own point of view (a second call is a
    /// no-op write); the *construction* of [`LiveSession`] itself happens
    /// exactly once, guarded by the screen's own `build`
    /// (`pages::TerminalScreen::build`) — never here, since a controller
    /// method has no reactive owner to build `Rc`-backed seams under (see
    /// [`LiveSession::enter`]'s own doc).
    ///
    /// **This entry never attaches to a real session**, even on a screen that
    /// could: it is the checked-in replay, and it stays that whatever is
    /// connected. [`attach_live`](Self::attach_live) is the other entry.
    pub fn enter_live(&self) {
        self.live.set(true);
    }

    /// Enter the live pane **attached to this screen's own `muxrd` session** —
    /// what a real, connected `/terminal?session=<id>` mounts instead of the
    /// mock transcript.
    ///
    /// Claims the pane for the attached session before flipping the flag, so
    /// the first `build` that observes it picks the real feed. A screen with
    /// no session id, or with no live client behind it, never calls this — its
    /// caller (`pages::TerminalScreen::init`) is the one place that decision is
    /// taken.
    pub fn attach_live(&self) {
        self.live_attached.store(true, Ordering::Relaxed);
        self.live.set(true);
    }

    /// The session the live pane should attach to, or `None` when the live
    /// pane is the demo replay (or there is no live pane at all) — what
    /// [`LiveSession::enter`] branches its feed on.
    pub fn attached_session(&self) -> Option<String> {
        if self.live_attached.load(Ordering::Relaxed) {
            self.session_id.clone()
        } else {
            None
        }
    }

    /// Mirror one poll of the attached feed onto the UI thread: publish its
    /// status, and re-bind the repository when the stream has minted a new
    /// connection id. See the [module docs](self)' "The link status is polled".
    ///
    /// **Read-compare-then-write**, exactly like
    /// [`refresh_appearance`](Self::refresh_appearance) and for the same
    /// reason: this runs on every wake of the repaint pump, and
    /// [`link`](Self::link) is a tracked read in the screen's `build`, so an
    /// unconditional `set` would wake a whole-app rebuild 30 times a second
    /// for the life of the mount. A status that has not moved writes nothing.
    pub fn mirror_link(&self, status: FeedStatus, connection_id: Option<String>) {
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
                .bind(self.session_id.clone(), Some(connection_id));
        }
    }

    /// Whether a tab or pane switch on this session has to be followed by a
    /// fresh attach.
    ///
    /// herdr's relay carries **only the focused pane** on an `AttachTerminal`
    /// stream, so a focus move the RPC performs is not something the open
    /// stream starts reflecting — the client has to attach again to be handed
    /// the new pane's output. zellij's relay carries the session, so its stream
    /// follows a switch on its own and a re-attach would only cost a
    /// screen-blanking round trip.
    ///
    /// Reads [`attached_session`](Self::attached_session), not merely the
    /// backend: a screen with no stream open has nothing to re-open.
    pub fn reattaches_on_switch(&self) -> bool {
        self.attached_session().as_deref().is_some_and(is_herdr)
    }

    /// Re-read the terminal-appearance prefs and update
    /// [`cursor_style`](Self::cursor_style)/[`font_size`](Self::font_size).
    /// Called once immediately on entering the live pane and then on
    /// [`APPEARANCE_POLL_INTERVAL`]'s own cadence for the rest of the mount
    /// (see [`LiveSession::enter`]). A failed read leaves both signals at
    /// whatever they last held — the same "stale beats wrong" choice the
    /// settings screen's own controller makes for its preference signals.
    ///
    /// Reads [`preferences`](Self::preferences) — the handle `pages.rs`
    /// resolved once at construction — rather than a `use_context` lookup:
    /// this runs on every poll tick, and a context lookup from inside a
    /// per-tick method is exactly the ad-hoc-DI pattern
    /// `docs/ARCHITECTURE.md`'s Building a Screen section rules out.
    ///
    /// **Read-compare-then-write**: `Set::set` notifies unconditionally, with
    /// no `PartialEq` gate of its own, and both signals are tracked reads in
    /// `pages::TerminalScreen::build` — so writing either one unconditionally
    /// on every tick would wake a whole-app rebuild + unpaced frame every
    /// [`APPEARANCE_POLL_INTERVAL`], forever, even while the live pane sits
    /// perfectly idle. Each signal is only `set()` when the freshly mapped
    /// value actually differs from its current [`get_untracked`](GetUntracked::get_untracked)
    /// reading — an idle poll (nothing changed since the last tick) writes
    /// neither, so it wakes no signal and no rebuild. Bumps
    /// [`appearance_writes`](Self::appearance_writes) exactly once per tick
    /// that wrote anything, for the test-only probe that pins this.
    pub async fn refresh_appearance(&self) {
        let Ok(prefs) = self.preferences.load_preferences().await else {
            return;
        };
        let mut wrote = false;
        let cursor = map_cursor_style(prefs.appearance.cursor_style);
        if self.cursor_style.get_untracked() != cursor {
            self.cursor_style.set(cursor);
            wrote = true;
        }
        let font_size = prefs.appearance.font_size_px as f32;
        if self.font_size.get_untracked() != font_size {
            self.font_size.set(font_size);
            wrote = true;
        }
        if wrote {
            self.appearance_writes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Test/diagnostic-only accessor for
    /// [`appearance_writes`](Self::appearance_writes) — see that field's own
    /// doc. Never called by production code.
    pub fn appearance_writes(&self) -> u64 {
        self.appearance_writes.load(Ordering::Relaxed)
    }
}

/// The backend prefix of a `<backend>:<name>` session id — see [`is_herdr`].
const HERDR_BACKEND: &str = "herdr:";

/// Drop the current `AttachTerminal` stream and open a fresh one at the grid's
/// current size — [`LiveSession::reattach`]'s shareable form.
///
/// An `Rc<dyn Fn()>` rather than a method because the *sheet* has to be able to
/// call it: its pages are pages of the root overlay host's navigator, which
/// sees `&mut AppState` and can never reach the screen's own state.
pub type Reattach = Rc<dyn Fn()>;

/// Whether the session with id `session` runs on the **herdr** backend — this
/// screen's one backend discriminator (see the [module docs](self)' "One
/// backend discriminator", which records both behaviours that hang off it).
///
/// Read off the id's own `<backend>:<name>` convention (`SessionInfo.id`'s own
/// documentation), which is the same fact the `sessions` feature reconciles a
/// rename against — no extra round trip, and one spelling of the convention
/// per feature.
fn is_herdr(session: &str) -> bool {
    session.starts_with(HERDR_BACKEND)
}

/// Flag the space named `label` selected among `spaces`, leaving every other
/// one's flag exactly as the server answered — the local half of
/// [`TerminalController::create_space`]'s courtesy pick. A `label` matching
/// nothing changes nothing: the server's own answer, whatever it is, wins.
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

/// The live pane's byte source, held **concretely** so the half of it that is
/// not on the [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed)
/// port stays reachable: the demo replay's test hooks on one side, and a real
/// session's status/connection-id/backoff controls on the other.
///
/// The port itself is deliberately unwidened — it is a byte stream, and a
/// status is not a byte — so this enum is where the two implementations stop
/// being interchangeable. Everything that only needs bytes takes
/// [`shared`](Self::shared)'s trait object instead.
pub enum LiveFeed {
    /// The checked-in fixture replay behind the overflow menu's "Demo
    /// session" row.
    Demo(Rc<RefCell<DemoFeed>>),
    /// A real `muxrd` session's `AttachTerminal` stream.
    Grpc(Rc<RefCell<GrpcTerminalFeed>>),
}

impl LiveFeed {
    /// This feed as the trait object the pump and the input seam take.
    ///
    /// A second handle onto the *same* cell, not a copy: replacing the feed
    /// through the concrete handle (a re-attach) is visible through every
    /// trait object minted here, which is what lets a session re-attach
    /// without rebuilding the pump or the seams around it.
    fn shared(&self) -> term::SharedFeed {
        match self {
            LiveFeed::Demo(feed) => feed.clone(),
            LiveFeed::Grpc(feed) => feed.clone(),
        }
    }

    /// The demo replay behind this feed, if that is what it is — the handle
    /// `pages::LiveProbe` exposes so a headless test can read
    /// [`DemoFeed::last_resize`]/`fed_bytes` off the mounted session.
    pub fn demo(&self) -> Option<&Rc<RefCell<DemoFeed>>> {
        match self {
            LiveFeed::Demo(feed) => Some(feed),
            LiveFeed::Grpc(_) => None,
        }
    }

    /// Where a real session's stream stands, or `None` for the demo replay
    /// (which has no link to report).
    pub fn status(&self) -> Option<FeedStatus> {
        match self {
            LiveFeed::Demo(_) => None,
            LiveFeed::Grpc(feed) => Some(feed.borrow().status()),
        }
    }
}

/// Everything the live pane needs: the emulator, the feed, the input and
/// selection seams, and the pump's repaint counter — constructed **once**, on
/// first entering the pane (see [`LiveSession::enter`]), and dropped when the
/// screen unmounts.
///
/// Deliberately **not** a [`TerminalController`] field: every handle here is
/// `Rc`-backed (`presentation::term`'s own module docs explain why — the
/// emulator/feed/seams are mutated during the event pass, where there is no
/// reactive owner), and embedding one would make the controller `!Send`/
/// `!Sync`, breaking `use_controller`'s own bound. It lives in the screen's
/// own `TerminalState` instead — the state-split rule's "non-reactive UI
/// state" case (`docs/ARCHITECTURE.md`'s Building a Screen section) —
/// constructed lazily by `Component::build` and read back via `&state.live`.
///
/// **Dropping it is the detach.** [`GrpcTerminalFeed`]'s own `Drop` shuts its
/// request stream down, so leaving the route — the one thing the overflow
/// menu's *Detach session* row does after the repository has answered — is
/// what the server sees as this client going away. There is no `Detach` RPC to
/// call instead (`data::repositories::grpc_terminal_repository`'s own docs).
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
    /// `presentation::term`'s module docs on the 30 Hz coalescing mechanism).
    pub generation: RwSignal<u64>,
    /// Drop this session's stream and open a fresh one at the grid's current
    /// size — `None` for the demo replay, which has no stream. Shared as an
    /// `Rc<dyn Fn()>` because the sheet, which is a page of a *different*
    /// navigator, has to be able to call it too (see `pages::Actions`).
    pub reattach: Option<Reattach>,
}

/// What re-opening an attached stream needs, beyond the feed handle itself.
struct AttachSpec {
    connection: Arc<ConnectionController>,
    session: String,
    /// The feed's auth-expired callback fires on the **background** runtime,
    /// where writing a signal is forbidden (`crate::connection`'s threading
    /// note), so it can only raise a flag; the UI-thread poll tick is what
    /// turns the flag into the connection controller's own teardown.
    auth_expired: Arc<AtomicBool>,
}

impl LiveSession {
    /// Construct the seams + feed and start the feed pump and the appearance
    /// poll.
    ///
    /// **Must run under the screen's own component owner** — i.e. only ever
    /// from `Component::build` (never a press handler, which runs under no
    /// owner at all on desktop, per `docs/ARCHITECTURE.md`'s Building a
    /// Screen section) — so `spawn_feed_pump`'s and `use_interval`'s
    /// `on_cleanup` registrations bind to the screen's own owner and actually
    /// fire on unmount, per those two functions' own "Ambient-Owner
    /// contract" docs. The caller (`pages::TerminalScreen::build`) guards
    /// this so it runs at most once per mount, matching the rebuild model's
    /// "construct once, not per build" rule.
    ///
    /// **Which feed** is [`TerminalController::attached_session`]'s answer,
    /// ANDed with the connection actually holding a client: a real session
    /// attaches, and everything else (the demo row, and the disconnected race
    /// the route guard normally prevents) replays the fixture rather than
    /// panicking on a client that is not there.
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
        // Wires the seam's synchronous echo drain to this session's own
        // emulator/generation pair (see `InputSeam::wire_pump`'s own doc) —
        // without this, `dispatch` sends but never drains, and a typed
        // keystroke would wait on `run_feed`'s next scheduled wake (or, once a
        // replay has drained, never surface at all).
        input.wire_pump(Rc::clone(&emulator), generation);
        let selection = term::SelectionSeam::new(Rc::clone(&emulator));

        let reattach = match (&feed, spec) {
            (LiveFeed::Grpc(grpc), Some(spec)) => {
                // The status/id/auth poll rides the repaint pump: it is
                // already awake, already on the UI thread, and already at the
                // rate the pane repaints (see the module docs).
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
                        mirror.mirror_link(status, connection_id);
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

        // One read right away — so the very first live frame already carries
        // the current prefs rather than the constructor's fallback defaults —
        // then keep polling for the rest of this mount.
        let now = Arc::clone(controller);
        spawn_local(async move { now.refresh_appearance().await });
        let poll = Arc::clone(controller);
        use_interval(APPEARANCE_POLL_INTERVAL, move || {
            let poll = Arc::clone(&poll);
            spawn_local(async move { poll.refresh_appearance().await });
        });

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

    /// The screen's *Retry*, offered on a disconnected link.
    ///
    /// Two cases, and the difference is whether the feed's own pump is still
    /// there: a schedule that merely ran out is **parked**, and
    /// `reset_backoff` re-arms that very session (its own doc); a session that
    /// ended for good — an expired credential — has no pump left to re-arm, so
    /// the only way back is a fresh stream.
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
}

impl AttachSpec {
    /// Open a stream for this session at a `cols` x `rows` grid.
    ///
    /// `None` when nothing is connected: the route guard normally keeps
    /// `/terminal` off an unauthenticated stack, but a connection torn down
    /// underneath a mounted screen is reachable, and it must degrade (to the
    /// fixture replay on entry, to "nothing re-attached" on a retry) rather
    /// than panic.
    fn attach(&self, cols: u16, rows: u16) -> Option<GrpcTerminalFeed> {
        let client = self.connection.client()?;
        let flag = Arc::clone(&self.auth_expired);
        Some(GrpcTerminalFeed::attach(
            client,
            self.session.clone(),
            cols,
            rows,
            Box::new(move || flag.store(true, Ordering::Release)),
        ))
    }

    /// The re-attach closure [`LiveSession::reattach`] holds: replace the
    /// feed's contents in place, so the pump and the seams built over the same
    /// cell keep working and the old stream is shut down by its own `Drop`.
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

/// Re-flagging the active tab in place, so the sheet's pills agree with the
/// card without a second load.
trait ActiveTab {
    /// Mark `key`'s tab active and every other tab not.
    fn update_active(&self, key: &str);
}

impl ActiveTab for RwSignal<Vec<TabEntry>> {
    fn update_active(&self, key: &str) {
        let tabs = self
            .get_untracked()
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
    // `#[cfg(test)]` exception, documented: this is the one place terminal's
    // `presentation` names a sibling feature's `data` module (rather than the
    // `terminal::domain` re-export every non-test import above goes through).
    // A concrete `SessionsRepository`/`PreferencesRepository` impl has to come
    // from *somewhere* to build a real `TerminalController` in a test, and
    // `sessions` itself already reaches its own `data` mock the identical way
    // from its own `#[cfg(test)]` module (huddle's convention: `di.rs` is the
    // only *production* site where `presentation` meets `data`). Test code
    // never ships, so this is not the layering edge `docs/ARCHITECTURE.md`
    // closes — it is test wiring standing in for the composition root.
    use crate::features::sessions::data::repositories::MockSessionsRepository;
    use crate::features::settings::data::repositories::MockPreferencesRepository;
    use crate::features::terminal::data::repositories::MockTerminalRepository;

    /// The test-wiring composition root: all three mock repositories behind
    /// their domain traits. `#[cfg(test)]`-only wiring, so `di.rs` stays the
    /// only production site where presentation meets `data` (huddle's
    /// convention).
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
        let c = controller(Some("s-1"));
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
            c.browsed_tab.get_untracked(),
            "devbox",
            "the sheet opens on the tab the session is on"
        );
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
            Some("s-1".to_string()),
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
        let attached = controller(Some("s-1"));
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
    fn only_a_herdr_session_re_attaches_on_a_switch() {
        // The stream carries the whole session on zellij and only the focused
        // pane on herdr, so only one of the two has to attach again.
        assert!(is_herdr("herdr:work"));
        assert!(!is_herdr("zellij:dev"));
        assert!(!is_herdr("dev"));

        let c = controller(Some("herdr:work"));
        assert!(
            !c.reattaches_on_switch(),
            "a screen that never attached has no stream to re-open"
        );
        c.attach_live();
        assert!(c.reattaches_on_switch());
    }

    #[test]
    fn a_tab_switch_reports_whether_it_landed() {
        let c = controller(Some("zellij:dev"));
        block_on(c.load());
        assert!(block_on(c.switch_tab("btop".to_string())));
        assert_eq!(c.active_tab.get_untracked(), "btop");
        assert_eq!(
            c.browsed_tab.get_untracked(),
            "btop",
            "the browse follows a move it drove itself"
        );
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

    /// A [`TerminalRepository`] that answers a two-tab, two-panes-apiece world
    /// and **records every call it is asked to make**, so a sheet flow's own
    /// RPC sequence — which calls, in which order, and which never happened at
    /// all — is assertable rather than inferred from the state it left behind.
    ///
    /// One double for both jobs: it also counts
    /// [`bind`](TerminalRepository::bind)s, the one call
    /// [`TerminalController::mirror_link`] makes that is observable at all.
    #[derive(Default)]
    struct RecordingRepository {
        calls: Mutex<Vec<String>>,
        last: Mutex<Option<String>>,
        /// The one verb that answers a failure instead of landing — how the
        /// "a refused step writes nothing" tests are driven.
        refuse: Mutex<Option<String>>,
    }

    impl RecordingRepository {
        /// Every call seen, oldest first.
        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// Only the calls that *move* something — the reads a load makes are
        /// noise in a sequence assertion about a commit.
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
            Ok(vec![
                Space {
                    id: "ws-1".to_string(),
                    name: "one".to_string(),
                    selected: true,
                },
                Space {
                    id: "ws-2".to_string(),
                    name: "two".to_string(),
                    selected: false,
                },
            ])
        }

        async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
            self.record("list_tabs".to_string())?;
            Ok(vec![
                recorded_tab(1, "alpha", true),
                recorded_tab(2, "beta", false),
            ])
        }

        async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure> {
            self.record(format!("tab_lines({key})"))?;
            Ok(Vec::new())
        }

        async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
            self.record(format!("switch_space({space_id})"))
        }

        async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
            self.record(format!("go_to_tab({tab_id})"))
        }

        async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
            self.record(format!("detach_session({id})"))
        }

        async fn focus_pane(&self, pane_id: u32, is_plugin: bool) -> Result<(), AppFailure> {
            self.record(format!("focus_pane({pane_id},{is_plugin})"))
        }

        async fn create_space(&self, label: String) -> Result<(), AppFailure> {
            self.record(format!("create_space({label})"))
        }

        async fn rename_space(&self, space_id: String, label: String) -> Result<(), AppFailure> {
            self.record(format!("rename_space({space_id},{label})"))
        }

        async fn new_tab(&self) -> Result<(), AppFailure> {
            self.record("new_tab".to_string())
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

    #[test]
    fn browsing_a_tab_touches_no_repository_at_all() {
        let (c, repository) = recorded("zellij:dev");
        let after_load = repository.calls();
        let lines = c.lines.get_untracked();
        let epoch = c.reveal_epoch.get_untracked();

        c.browse_tab("beta".to_string());
        c.browse_tab("alpha".to_string());
        c.browse_tab("beta".to_string());

        assert_eq!(c.browsed_tab.get_untracked(), "beta");
        assert_eq!(
            c.active_tab.get_untracked(),
            "alpha",
            "browsing moves the sheet, never the session"
        );
        assert_eq!(repository.calls(), after_load, "no RPC, none at all");
        assert_eq!(c.lines.get_untracked(), lines, "…and no card change");
        assert_eq!(c.reveal_epoch.get_untracked(), epoch, "…and no replay");
    }

    #[test]
    fn opening_the_sheet_points_the_browse_back_at_the_session() {
        let (c, _repository) = recorded("zellij:dev");
        c.browse_tab("beta".to_string());

        c.begin_browsing();

        assert_eq!(c.browsed_tab.get_untracked(), "alpha");
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
        c.browse_tab("beta".to_string());

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
    fn a_herdr_commit_re_attaches_instead_of_calling_focus_pane() {
        let (c, repository) = recorded("herdr:work");
        c.attach_live();
        c.browse_tab("beta".to_string());

        assert!(block_on(c.commit_pane("beta".to_string(), 21, false)));

        assert_eq!(
            repository.actions(),
            ["go_to_tab(2)"],
            "on herdr the re-attach is the focus, so the RPC is a no-op"
        );
        assert!(
            c.reattaches_on_switch(),
            "…and the caller is told to re-open the stream"
        );
        assert_eq!(focused_pane(&c, "beta"), Some(21));
    }

    #[test]
    fn a_zellij_commit_never_re_attaches() {
        let (c, _repository) = recorded("zellij:dev");
        c.attach_live();
        assert!(block_on(c.commit_pane("alpha".to_string(), 11, false)));
        assert!(
            !c.reattaches_on_switch(),
            "zellij's stream follows the server's own focus"
        );
    }

    #[test]
    fn a_refused_tab_step_stops_the_commit_before_the_pane_focus() {
        let (c, repository) = recorded("zellij:dev");
        repository.refuse("go_to_tab");
        c.browse_tab("beta".to_string());

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
            c.browsed_tab.get_untracked(),
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
    fn a_new_tab_reloads_the_workspace_with_no_dialog_involved() {
        let (c, repository) = recorded("zellij:dev");

        block_on(c.new_tab());

        assert_eq!(
            repository.actions(),
            ["new_tab"],
            "one RPC — the reload behind it is reads, filtered out of `actions`"
        );
        assert_eq!(
            c.browsed_tab.get_untracked(),
            "alpha",
            "the reload re-seeded the browse onto the server-active tab"
        );
    }

    #[test]
    fn a_refused_new_tab_reloads_nothing() {
        let (c, repository) = recorded("zellij:dev");
        repository.refuse("new_tab");
        let tabs = c.tabs.get_untracked();

        block_on(c.new_tab());

        assert_eq!(repository.actions(), ["new_tab"]);
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

    #[test]
    fn the_live_pane_seeds_the_shipped_appearance_defaults() {
        // Before the first `refresh_appearance` lands, the very first live
        // frame must still show the shipped default rather than a blank/wrong
        // shape — see settings' own `Preferences::defaults()`.
        let c = controller(None);
        assert_eq!(c.cursor_style.get_untracked(), term::CursorStyle::Block);
        assert_eq!(c.font_size.get_untracked(), DEFAULT_LIVE_FONT_SIZE);
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
}
