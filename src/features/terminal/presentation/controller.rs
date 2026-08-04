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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use clean_signals::{ControllerCore, RunOptions};
use clean_signals_frust::use_interval;
use frust::{GetUntracked, RwSignal, Set, spawn_local};

use crate::failure::AppFailure;
use crate::features::terminal::data::feeds::demo_feed::DemoFeed;
use crate::features::terminal::domain::entities::{
    RenameRequest, SettingsCursorStyle, Space, TabEntry, TermLine, Workspace,
};
use crate::features::terminal::domain::repositories::{
    PreferencesRepository, SessionsRepository, TerminalRepository,
};
use crate::features::terminal::domain::use_cases::{
    DetachSession, LoadWorkspace, RenameAttachedSession, SwitchTab,
};

use super::term;

/// The `/terminal` screen's view model. See the [module docs](self).
pub struct TerminalController {
    core: ControllerCore<AppFailure>,
    load: LoadWorkspace,
    switch: SwitchTab,
    rename: RenameAttachedSession,
    detach: DetachSession,
    /// The attached session's id (`/terminal?session=<id>`), or `None` for a
    /// bare `/terminal`. Plain data: it is fixed for this mount.
    session_id: Option<String>,
    /// The sheet's `spaces` pill row.
    pub spaces: RwSignal<Vec<Space>>,
    /// The sheet's `tabs` pill row.
    pub tabs: RwSignal<Vec<TabEntry>>,
    /// The key of the tab the card is showing.
    pub active_tab: RwSignal<String>,
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
    /// Whether the card shows the live demo pane instead of the mock
    /// transcript — see the [module docs](self)'s "Pane-content switch"
    /// note and [`LiveSession`]. Entered once per mount, through the
    /// overflow menu's "Demo session" row (`pages::Actions::enter_demo`);
    /// there is no path back to the mock transcript within the same mount.
    pub live: RwSignal<bool>,
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
        Self {
            core: ControllerCore::new(),
            load: LoadWorkspace::new(Arc::clone(&terminal), Arc::clone(&sessions)),
            switch: SwitchTab::new(Arc::clone(&terminal)),
            rename: RenameAttachedSession::new(sessions),
            detach: DetachSession::new(terminal),
            session_id,
            spaces: RwSignal::new(Vec::new()),
            tabs: RwSignal::new(Vec::new()),
            active_tab: RwSignal::new(String::new()),
            lines: RwSignal::new(Vec::new()),
            session_name: RwSignal::new(None),
            loaded: RwSignal::new(false),
            fullscreen: RwSignal::new(false),
            reveal_epoch: RwSignal::new(0),
            next_epoch: AtomicU64::new(1),
            live: RwSignal::new(false),
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
    pub async fn load(&self) {
        let Ok(workspace) = self
            .core
            .run(&self.load, self.session_id.clone(), RunOptions::default())
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
        self.active_tab.set(active);
        self.lines.set(lines);
        self.session_name.set(session_name);
        self.loaded.set(true);
        self.replay_reveal();
    }

    /// Switch the card to the tab keyed `key`: its lines replace the body's
    /// and the reveal replays from the top (the mock's own behaviour —
    /// `renderTab` ends in `playReveal`).
    ///
    /// Switching to the tab already showing still replays, which is what makes
    /// this the one path a "replay" affordance would reuse.
    pub async fn switch_tab(&self, key: String) {
        let Ok(lines) = self
            .core
            .run(&self.switch, key.clone(), RunOptions::default())
            .await
        else {
            return;
        };
        self.active_tab.set(key.clone());
        self.tabs.update_active(&key);
        self.lines.set(lines);
        self.replay_reveal();
    }

    /// Select the space named `name` — selection state only, in the mock era
    /// (nothing else on screen depends on which space is current).
    pub fn select_space(&self, name: &str) {
        let spaces = self
            .spaces
            .get_untracked()
            .into_iter()
            .map(|space| Space {
                selected: space.name == name,
                ..space
            })
            .collect();
        self.spaces.set(spaces);
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

    /// Enter the live demo pane. Idempotent from this signal's own point of
    /// view (a second call is a no-op write); the *construction* of
    /// [`LiveSession`] itself happens exactly once, guarded by the screen's
    /// own `build` (`pages::TerminalScreen::build`) — never here, since a
    /// controller method has no reactive owner to build `Rc`-backed seams
    /// under (see [`LiveSession::enter`]'s own doc).
    pub fn enter_live(&self) {
        self.live.set(true);
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

/// Everything the live demo pane needs: the emulator, the feed, the input and
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
pub struct LiveSession {
    /// The VT emulator the grid renders and the selection resolves against.
    pub emulator: term::SharedEmulator,
    /// The byte source — concrete (not the `SharedFeed` trait object) so a
    /// test can read [`DemoFeed::last_resize`], the resize test hook, without
    /// a widget-level handle (see `pages::LiveProbe`).
    pub feed: Rc<RefCell<DemoFeed>>,
    /// The grid's (and the key bar's) shared keystroke source.
    pub input: term::InputSeam,
    /// The grid's (and the Copy chip's) shared selection gesture.
    pub selection: term::SelectionSeam,
    /// The feed pump's repaint counter — a **tracked** read of this in
    /// `Component::build` is what wakes the live pane's repaint (see
    /// `presentation::term`'s module docs on the 30 Hz coalescing mechanism).
    pub generation: RwSignal<u64>,
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
    pub fn enter(controller: &Arc<TerminalController>) -> Self {
        let emulator: term::SharedEmulator =
            Rc::new(RefCell::new(term::TerminalEmulator::with_defaults()));
        let feed: Rc<RefCell<DemoFeed>> = Rc::new(RefCell::new(DemoFeed::new()));
        let feed_dyn: term::SharedFeed = feed.clone();
        let generation = RwSignal::new(0u64);
        let mut input = term::InputSeam::new(feed_dyn.clone());
        // Task 06: wires the seam's synchronous echo drain to this session's
        // own emulator/generation pair (see `InputSeam::wire_pump`'s own
        // doc) — without this, `dispatch` sends but never drains, and a
        // typed keystroke would wait on `run_feed`'s next scheduled wake
        // (or, once the replay has drained, never surface at all).
        input.wire_pump(Rc::clone(&emulator), generation);
        let selection = term::SelectionSeam::new(Rc::clone(&emulator));
        term::spawn_feed_pump(feed_dyn, Rc::clone(&emulator), generation);

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
        }
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
        assert_eq!(c.spaces.get_untracked().len(), 3);
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
        let c = controller(None);
        block_on(c.load());

        c.select_space("cupline");

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
