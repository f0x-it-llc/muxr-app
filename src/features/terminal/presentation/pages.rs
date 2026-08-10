//! The `/terminal` route's page: muxr's terminal surface, a pixel copy of
//! `docs/terminal.html` **v2** ("One less row").
//!
//! [`terminal_page`] is what `crate::routes` mounts; everything below it is
//! the canonical screen shape `docs/ARCHITECTURE.md`'s **Building a Screen**
//! section mandates — a per-screen [`Component`] ([`TerminalScreen`]) whose
//! `init` hosts a [`TerminalController`] through
//! [`use_controller`](clean_signals_frust::use_controller), so the controller
//! is created on route mount and disposed on unmount, and whose `build` is
//! pure arrangement over [`super::pieces`]' elements.
//!
//! # What v2 removed
//!
//! v2 supersedes `docs/redesign.html`'s terminal blocks outright: **no** tabs
//! strip, **no** path bar, **no** cmd hint, **no** FAB, **no** keyboard bar.
//! The two jobs the old terminal-head row did moved: the overflow `⋮` is on
//! the *app* bar now (`crate::routes::BarAction::TerminalMenu`, forwarded by
//! `crate::shell` to [`open_terminal_menu`]), and the pane title moved into the
//! card's own status row beside the one button that toggles fullscreen.
//!
//! ```text
//! ┌──────────────────────────────────────┐
//! │ ‹        terminal               ⋮    │  crate::shell's bar — NOT this
//! ├──────────────────────────────────────┤  screen's; it floats over it
//! │  ┌────────────────────────────────┐  │  ← .term-wrap, 14px, animated
//! │  │ ed@devbox: ~               ⛶  │  │  ← .term-statusrow
//! │  │ ed@devbox:~$ systemctl status  │  │
//! │  │ Active: active (running) …     │  │  ← .term-body, staggered in
//! │  │ # staggered per-line reveal …  │  │
//! │  │ ed@devbox:~$ ▮                 │  │  ← the blinking caret
//! │  └────────────────────────────────┘  │
//! └──────────────────────────────────────┘
//! ```
//!
//! # Fullscreen: one flag, three consumers
//!
//! Tapping the card's own `⛶` flips [`TerminalController::fullscreen`], and
//! three separate things follow from it — which is exactly why it is
//! controller state rather than widget state:
//!
//! 1. the **card** animates its top clearance (`safe-area + BAR_HEIGHT` →
//!    `safe-area`), its wrap padding (14 → 0) and its corner radius (22 → 0),
//!    all in one gesture (`super::chrome::terminal_stage`);
//! 2. the **button** swaps its vector mark and takes the amber on-state, in
//!    the same on-screen position throughout ("one button, always in the same
//!    place");
//! 3. the **shell's bar** animates away, by publishing the flag through
//!    [`Nav::set_fullscreen`] — the bar's *existing* hidden/reveal driver, not
//!    a second hide path.
//!
//! **Unmount restores the bar.** The flag lives beside the app's route-meta
//! mirror, so it outlives this screen unless something clears it; `init`
//! registers an [`on_cleanup`] that does, which is what stops a back-nav out
//! of fullscreen from stranding a hidden bar on `/sessions`.
//!
//! # OS system UI: edge-triggered, never build-time
//!
//! The three consumers above are all *build-time*, re-derived from
//! [`TerminalController::fullscreen`] every rebuild — cheap because a mobile
//! rebuild runs every frame and each of them is a plain layout/paint read.
//! `frust::set_system_ui_mode` is not that kind of call: it bumps a
//! process-global generation counter a shell watcher polls once per frame,
//! so driving it from `build` would re-request the same mode every single
//! frame this screen is mounted. So hiding/restoring the OS status and
//! navigation bars (Flutter `SystemChrome.setEnabledSystemUIMode` parity) is
//! instead a **one-shot call at the state-transition edge**, never a
//! reactive `build`-time read: [`Actions::toggle_fullscreen`] itself
//! requests [`SystemUiMode::ImmersiveSticky`] on expand (a swipe reveals the
//! bars transiently, then they auto-hide again — the shadertoy precedent,
//! right for a terminal) and calls [`restore_system_ui`] on collapse. Every
//! other exit — a back-nav pop, Detach's landing on `/sessions`, a
//! session-guard bounce/disconnect elsewhere — is an *unmount*, not a
//! `toggle_fullscreen` call, so it is covered the same way the bar's own
//! `bar_fullscreen.set(false)` already is: `init`'s [`on_cleanup`] calls
//! [`restore_system_ui`] unconditionally, since `ComponentWidget::teardown`
//! disposing this screen's `Owner` is the one choke point every one of those
//! exits passes through.
//!
//! # The component-state boundary, and the app bar's press
//!
//! A `Component`'s subtree is bound to its own `State`, so this screen's own
//! gestures take `&mut TerminalState`. Its **overlays** are a different story:
//! the menu, the sheet and the rename dialog are pages on the *app's* root
//! overlay host (`crate::AppState::overlays` — above every piece of chrome,
//! which is what stopped the `⋮` menu clipping under the floating bar and the
//! bar staying tappable beneath it, FINDINGS #44), so their callbacks receive
//! `&mut AppState` and cannot see
//! [`TerminalState`] at all. That is why every action lives on [`Actions`] — a
//! small clonable bundle of handles either side can capture and act through
//! (the `features::sessions` precedent).
//!
//! The app bar's `⋮` is one step further out still: `crate::shell` builds that
//! bar from route metadata and its press handler sees only `&mut AppState`. So
//! this feature publishes its [`Actions`] in a process-`thread_local!` while
//! the screen is mounted ([`open_terminal_menu`] is the one reader) — the same
//! handle-in-a-thread-local idiom `crate::routes::app_nav` and
//! `crate::ui::toasts::app_toasts` document, and for the same underlying
//! reason: `provide_context` requires `Send + Sync`, which the `Rc`-backed
//! `NavigatorController` in the bundle can never be.
//!
//! # Pane-content switch: two ways in, and a third only a test takes
//!
//! [`TerminalScreen::build`] branches the card's body (and the key bar's
//! mount) on one flag, [`TerminalController::live`], and **two** things set
//! it on a device:
//!
//! 1. **A real attached session.** `init` flips it — through
//!    [`TerminalController::attach_live`], which also claims the pane for the
//!    session — when this screen carries a `?session=<id>` *and* the app holds
//!    a live client. That is the production path: a real `muxrd` session has no
//!    transcript to describe, it has a stream, so it mounts
//!    [`term::terminal_surface`] over a `GrpcTerminalFeed` from the first
//!    frame.
//! 2. **Nothing.** A bare `/terminal`, and every headless suite (whose
//!    connection is mock-backed and hands out no client), keep
//!    [`pieces::card_body`]'s seeded mock transcript — the content
//!    `tests/terminal.rs` pins end to end through this real composition root.
//!
//! The third is the **checked-in fixture replay** (`data::feeds::DemoFeed`),
//! which no longer has a row of its own in the overflow menu: it is reached by
//! calling [`TerminalController::enter_live`] on the mounted screen's own
//! controller, published for that purpose by [`debug_controller`], and
//! `tests/terminal_live.rs` is its one caller. Same controller call the menu's
//! removed *Demo session* row made, so everything below it — the seams, the
//! feed, the pump, the grid — is the very composition a device runs.
//!
//! [`LiveSession`] — the seams, the feed and the pump — is constructed
//! **once**, lazily, inside `build` itself (guarded by `state.live.is_none()`
//! — see [`TerminalScreen::build`]), not from the menu's own press handler:
//! `Component::build` runs under this screen's own reactive `Owner`
//! (`ComponentWidget::rebuild` wraps it in `owner.with`), which is what makes
//! `spawn_feed_pump`'s and `clean_signals_frust::use_interval`'s own
//! `on_cleanup` registrations bind to *this* screen and actually run on
//! unmount — a press handler runs under no owner at all on desktop, and an
//! `on_cleanup` registered there silently leaks (both functions' own
//! "Ambient-Owner contract" docs).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::authoring::{Rect, WindowInsets};
use frust::glyph::{MenuEntry, menu_item, show_glyph_menu};
use frust::{
    AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Get, GetUntracked,
    NavigatorController, Padding, PopResult, SystemUiMode, WindowMetrics, any, button, component,
    flexible, inflexible, on_cleanup, set_system_ui_mode, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::settings::di::expect_preferences_repository;
use crate::features::terminal::data::feeds::demo_feed::DemoFeed;
use crate::features::terminal::data::feeds::grpc_feed::FeedStatus;
use crate::features::terminal::di::expect_terminal_repository;
use crate::features::terminal::domain::entities::TabEntry;
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::name_input_dialog;
use crate::ui::sheet::{SheetHandle, show_sheet};
use crate::ui::toasts::app_toasts;

use super::chrome::{WRAP_PAD, status_strip, term_box};
use super::controller::{LiveSession, Reattach, TerminalController};
use super::keybar;
use super::pieces::{
    self, PANE_ROW_GAP, PILL_GAP, Palette, PillState, SECTION_BOTTOM_GAP, SECTION_TOP_GAP,
};
use super::term;

/// How long the sheet stays up after a **pane row** is tapped, so the selection
/// visibly moves before it closes — `setTimeout(closeSheet, 260)` in the mock.
///
/// A pane tap is the sheet's one dismissing gesture (see
/// [`Actions::commit_pane`]); browsing a tab or a space leaves the sheet up
/// indefinitely.
const SHEET_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(260);

/// The overflow menu's rows, in order. Indices into the entry list
/// [`show_glyph_menu`] hands back, so they are named once rather than spelled
/// at the match site — and, with no separators left in the menu (see
/// [`menu_entries`]), a row's index is now simply its position.
const MENU_PANES: &str = "Panes & tabs";
const MENU_RENAME: &str = "Rename session";
const MENU_DETACH: &str = "Detach session";
const MENU_PANES_INDEX: usize = 0;
const MENU_RENAME_INDEX: usize = 1;
const MENU_DETACH_INDEX: usize = 2;

/// The rename dialog's title.
const RENAME_TITLE: &str = "Rename session";
/// What the two UI-only affordances (the add pills, the new-pane row) say.
const UNAVAILABLE_TOAST: &str = "not yet available";
/// …and what a landed detach says on the way back to the list.
const DETACHED_TOAST: &str = "detached";
/// What a control a read-only credential may not use says when it is reached
/// anyway (the overflow menu has no disabled row state, so *Rename session*
/// stays on screen and refuses — see [`Actions::open_rename_dialog`]).
const READ_ONLY_TOAST: &str = "read-only session";

/// The link strip's three messages, and the two affordances beside them — see
/// [`link_strip`].
const LINK_RECONNECTING: &str = "reconnecting\u{2026}";
const LINK_DISCONNECTED: &str = "disconnected";
const LINK_CLOSED: &str = "session ended";
const LINK_RETRY: &str = "Retry";
const LINK_LEAVE: &str = "Back to sessions";

/// The sheet's three section labels. The third names the tab whose panes it is
/// listing (`panes — devbox`).
const SECTION_SPACES: &str = "spaces";
const SECTION_TABS: &str = "tabs";
const SECTION_PANES: &str = "panes \u{2014} ";
/// The two "add" pills (`+ space` / `+`) — content text inside a pill, not
/// chrome, so these stay runs rather than becoming vectors.
const ADD_SPACE: &str = "+ space";
const ADD_TAB: &str = "+";

/// The query key `/terminal?session=<id>` carries the attached session's id
/// under.
///
/// Transcribed rather than imported: the `/sessions` screen spells the same
/// key when it pushes, and a feature never imports a sibling's `presentation`
/// (`docs/ARCHITECTURE.md`'s cross-feature rule). `tests/terminal.rs` asserts
/// the round trip, which is what keeps the two spellings in step.
const SESSION_QUERY_KEY: &str = "session";

/// The wrap padding's wide-window target (FINDINGS #21's conservative
/// clamp/padding-scaling pass) — `super::chrome::WRAP_PAD`'s (14px) windowed
/// value, scaled up past [`WIDE_BREAKPOINT_PX`].
///
/// **The card itself stays full-bleed at any width — this screen gets no
/// content-column clamp**, unlike the other three: only the breathing room
/// around the card grows. Applied as an *extra* [`Padding`] wrapping
/// `terminal_stage` from the outside, rather than widening `WRAP_PAD` itself,
/// because that constant also drives the fullscreen transition's
/// animated-to-zero geometry — widening it would widen the fullscreen target
/// too, and fullscreen must keep the card edge-to-edge at any width.
const WIDE_WRAP_PAD_PX: f64 = 22.0;

/// The portrait window's bar-to-card breathing room: a **top-only** gap
/// between the floating bar and the card, applied at the same site (and in the
/// same outside-the-stage way) as [`WIDE_WRAP_PAD_PX`].
///
/// A wide window has read as spaced since the responsive pass, because its
/// all-round step-up lifts the card off the bar as a side effect; a narrow one
/// took no extra padding at all and sat flush under it. The card's own
/// clearance (`super::chrome`'s `insets.top + BAR_HEIGHT·(1−p)`) is
/// orientation-independent and stays that way — the gap belongs to the same
/// responsive branch that created the difference. Deliberately the wide
/// branch's *own* extra (`WIDE_WRAP_PAD_PX − WRAP_PAD`), so both orientations
/// breathe by the same amount at the top; the sides keep the narrow window's
/// [`WRAP_PAD`], and fullscreen takes neither (the stage's own progress
/// animation owns that geometry).
const PORTRAIT_TOP_GAP_PX: f64 = WIDE_WRAP_PAD_PX - WRAP_PAD;

const _: () = {
    assert!(WIDE_WRAP_PAD_PX > WRAP_PAD);
    // …which is the portrait gap too, so neither orientation can end up
    // sitting flush under the bar again.
    assert!(PORTRAIT_TOP_GAP_PX > 0.0);
};

/// The Terminal screen's route entry point: a `Component` under the outer
/// [`AppState`], so its `init` runs under a real reactive `Owner` that
/// disposes the hosted controller on unmount.
pub fn terminal_page(overlays: NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(TerminalScreen { overlays }))
}

/// The Terminal screen `Component`. Stateless configuration — everything
/// mutable lives in [`TerminalState`] and the controller it hosts.
pub struct TerminalScreen {
    /// The root overlay host's navigator, handed in by the route builder.
    overlays: NavigatorController<AppState>,
}

/// The Terminal screen's retained state: the action bundle, and the live
/// demo pane once one has been entered.
///
/// The [state-split rule](crate::features::terminal::presentation::controller)
/// in practice — the card's animation drivers are inside their own retained
/// widgets, the observed state is in the controller's signals, and what is
/// left here is the handles a press handler reads plus [`live`](Self::live):
/// the `Rc`-backed seams/feed [`LiveSession`] embeds cannot live on the
/// controller at all (see that type's own doc), so this is where they land —
/// `None` for the mock-only lifetime of a screen that never enters the demo
/// pane, `Some` from the first `build` that observes
/// `TerminalController::live` and never `None` again for this mount.
pub struct TerminalState {
    /// Everything this screen can *do*, and the handles it needs to do it.
    pub actions: Actions,
    live: Option<LiveSession>,
}

/// Every navigation and mutation this screen performs, plus the handles they
/// need.
///
/// Cloneable on purpose: the card's own gestures reach these through
/// `&mut TerminalState`, while a *sheet row*, a *menu entry* and the *app
/// bar's `⋮`* — all of which only ever see `&mut AppState`, or no state at all
/// — capture a clone and call the very same method. One body, every side of
/// the component boundary.
#[derive(Clone)]
pub struct Actions {
    /// Which mount of the screen this bundle belongs to — see
    /// [`TERMINAL_CHROME`].
    id: u64,
    /// The root overlay host's navigator — every page *builder* this screen
    /// pushes (the menu, the sheet, the rename dialog) goes through it, so
    /// each lands above the app bar it is anchored to rather than under it
    /// (`crate::AppState::overlays`).
    pub overlays: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how this screen pops back to
    /// `/sessions`, and how it publishes the bar's fullscreen flag.
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<TerminalController>,
    /// The app's one connection handle — read for the live client the feed
    /// attaches with, and for the read-only posture that decides which of this
    /// screen's controls are offered at all.
    pub connection: Arc<ConnectionController>,
    /// The mounted live session's re-attach hook, once one exists — see
    /// [`LiveSession::reattach`]. Filled in by `build` (which is where the
    /// session is constructed) and read from the **sheet**, whose pages see
    /// `&mut AppState` and can never reach `TerminalState`; shared
    /// (`Rc<RefCell<_>>`, like [`live_probe`](Self::live_probe)) so every
    /// clone of this bundle — the thread-local's included — sees the same
    /// fill.
    reattach: Rc<RefCell<Option<Reattach>>>,
    /// Where the app bar's trailing `⋮` slot is painting, in window
    /// coordinates — written by `super::chrome::terminal_stage` on every
    /// layout, read when the menu opens. See [`Actions::open_menu`].
    pub anchor: Rc<Cell<Rect>>,
    /// Whether a detach has already claimed this screen's one pop — see
    /// [`Actions::detach`]. Shared (`Rc`) across every clone of the bundle, so
    /// the menu, the sheet and the bar are all talking about the same latch:
    /// one mount of this screen leaves `/terminal` at most once.
    detaching: Rc<Cell<bool>>,
    /// A test/diagnostic mirror of the live demo pane's emulator + feed, if
    /// one has been entered — see [`LiveProbe`] and [`debug_live_probe`].
    /// `Actions` is cloned into [`TERMINAL_CHROME`] at `init`, before any
    /// `LiveSession` exists; this stays `Rc<RefCell<_>>` (rather than a plain
    /// `Option`) so `TerminalScreen::build` can fill it in later and have
    /// every existing clone — the thread-local's included — see the same
    /// fill.
    live_probe: Rc<RefCell<Option<LiveProbe>>>,
}

/// A test/diagnostic handle onto the live demo pane's emulator + feed —
/// reached through the mounted screen's real composition root
/// ([`debug_live_probe`]) rather than a widget-level handle, so a headless
/// test can assert byte-correct replay/typing (`vt100::Screen::cell`, the
/// same accessor `presentation::term`'s own unit tests use) and the resize
/// test hook (`DemoFeed::last_resize`) against the actual mounted session.
#[derive(Clone)]
pub struct LiveProbe {
    /// The VT emulator the live grid is rendering.
    pub emulator: term::SharedEmulator,
    /// The demo feed the grid's pump is draining — concrete, so
    /// [`DemoFeed::last_resize`] and [`DemoFeed::fed_bytes`] are reachable.
    ///
    /// `None` on a live **`muxrd`** session, whose feed is a real stream with
    /// no fixture behind it: what a test asserts about *that* is the link
    /// status the screen mirrors, not a replay cursor.
    pub feed: Option<Rc<RefCell<DemoFeed>>>,
}

/// The mounted screen's live pane, if one has been entered — see
/// [`LiveProbe`]. `None` before the pane is entered (see the [module
/// docs](self)'s "Pane-content switch"), or when no terminal screen is mounted
/// at all.
pub fn debug_live_probe() -> Option<LiveProbe> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|actions| actions.live_probe.borrow().clone())
    })
}

thread_local! {
    /// Test-only override for the live pane's Copy sink — see
    /// `term::chip`'s own doc on why production never swaps this out itself.
    /// `None` (the default) leaves `term::copy_chip`'s system-clipboard sink
    /// in place. Set with [`set_demo_copy_sink`] before mounting the screen.
    static DEMO_COPY_SINK: RefCell<Option<term::CopySink>> = const { RefCell::new(None) };
}

/// Override the live demo pane's clipboard sink for a headless test, so
/// `Copy` can be asserted without touching the real OS clipboard. Pass `None`
/// to restore the production sink.
pub fn set_demo_copy_sink(sink: Option<term::CopySink>) {
    DEMO_COPY_SINK.with(|cell| *cell.borrow_mut() = sink);
}

/// The current override, if a test installed one — see [`set_demo_copy_sink`].
fn demo_copy_sink() -> Option<term::CopySink> {
    DEMO_COPY_SINK.with(|cell| cell.borrow().clone())
}

thread_local! {
    /// The mounted terminal screen's [`Actions`], or `None` when no terminal
    /// screen is on the stack — see the [module docs](self) for why this is a
    /// thread-local rather than context.
    ///
    /// Tagged with the mount's own [`Actions::id`] so a cleanup only ever
    /// withdraws **its own** entry. Frust builds an incoming page before
    /// tearing the outgoing one down, so a screen that unmounted after a
    /// second one published would otherwise blank the live screen's `⋮` — a
    /// state no user could recover from without leaving the route.
    static TERMINAL_CHROME: RefCell<Option<Actions>> = const { RefCell::new(None) };
}

/// Mints [`Actions::id`]s. Process-wide and monotonic; the value itself never
/// leaves this module.
static NEXT_CHROME_ID: AtomicU64 = AtomicU64::new(1);

/// Test/diagnostic accessor: the mounted screen's controller, if any — reached
/// through the same [`TERMINAL_CHROME`] handle [`open_terminal_menu`] already
/// reads, so a headless test talks to the *mounted* screen's own view model
/// rather than one it built itself.
///
/// Two things use it, both from `tests/`: reading
/// `TerminalController::cursor_style`/`font_size`/`live` (the
/// settings-reflect-into-the-live-pane acceptance test), and calling
/// [`TerminalController::enter_live`] to mount the checked-in fixture replay —
/// the **test seam** that replaced the overflow menu's removed *Demo session*
/// row (see the [module docs](self)'s "Pane-content switch"). Reusing this one
/// published handle is deliberate: `tests/*.rs` are integration binaries, so a
/// `#[cfg(test)]` hook on the lib would be invisible to them, and every
/// alternative widens the production API instead of borrowing the accessor that
/// already exists.
pub fn debug_controller() -> Option<Arc<TerminalController>> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|actions| Arc::clone(&actions.controller))
    })
}

/// Open the terminal's overflow menu — what `crate::shell` forwards the app
/// bar's `⋮` press to.
///
/// The feature owns its own menu; the shell only forwards (the same split the
/// `⚙`'s route push follows). A press that arrives with no terminal screen
/// mounted is silently ignored: the bar only ever carries this action on
/// `/terminal`, so that is an unreachable race rather than a state to handle.
pub fn open_terminal_menu(_state: &mut AppState) {
    let actions = TERMINAL_CHROME.with(|cell| cell.borrow().clone());
    if let Some(actions) = actions {
        actions.open_menu();
    }
}

impl Component for TerminalScreen {
    type State = TerminalState;

    fn init(&self) -> TerminalState {
        // Every app-scoped handle, resolved here and stored — never from
        // inside a press handler.
        let app_nav = app_nav();
        // The attached session's id. Read **here**, in `init`, because that is
        // the one moment `Nav::param` is exact: this page is being built by
        // the very push that recorded the query (see that method's docs for
        // why the query cannot arrive through the route builder at all).
        let session = app_nav.param(SESSION_QUERY_KEY);
        // The screen-scoped view model, over the three app-scoped
        // repositories `di` published — `terminal`'s own, plus `sessions`'
        // and `settings`' resolved directly through their own `di.rs`
        // (`docs/ARCHITECTURE.md`'s Feature Slices section records both as
        // the same `di.rs`-only widening). Resolved exactly **once**, here,
        // not from inside `TerminalController::refresh_appearance`'s
        // per-tick poll — the ad-hoc-DI pattern that method used to follow.
        // Auto-disposed on unmount via `on_cleanup`.
        let controller = use_controller::<TerminalController, AppFailure>(move || {
            TerminalController::new(
                expect_terminal_repository(),
                expect_sessions_repository(),
                expect_preferences_repository(),
                session.clone(),
            )
        });

        // Failure → toast, for this screen's lifetime.
        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        // The one place the mock-vs-live decision is taken (see the module
        // docs' "Pane-content switch"): a screen that carries a session id
        // *and* has a live client behind it is showing a real terminal, so it
        // mounts the grid from the start rather than a transcript that
        // describes nothing. Everything else — a bare `/terminal`, and every
        // headless suite, whose connection is mock-backed and hands out no
        // client — keeps the mock transcript.
        let connection = expect_connection_controller();
        if controller.session_id().is_some() && connection.client().is_some() {
            controller.attach_live();
        }

        // First load: spaces, tabs, the active tab's lines, the session name.
        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.load().await });

        let actions = Actions {
            id: NEXT_CHROME_ID.fetch_add(1, Ordering::Relaxed),
            overlays: self.overlays.clone(),
            app_nav,
            controller,
            connection,
            reattach: Rc::new(RefCell::new(None)),
            anchor: Rc::new(Cell::new(Rect::ZERO)),
            detaching: Rc::new(Cell::new(false)),
            live_probe: Rc::new(RefCell::new(None)),
        };
        // Publish for the app bar's `⋮` (see the module docs).
        TERMINAL_CHROME.with(|cell| *cell.borrow_mut() = Some(actions.clone()));

        // `on_cleanup`'s closure is `Send + Sync`-bounded, which the `Rc`-backed
        // `Nav` can never satisfy — hence the flag's own small handle (see
        // `crate::routes::BarFullscreen`). Withdrawing the published chrome
        // captures nothing but a `u64`, for the same reason.
        let bar_fullscreen = actions.app_nav.fullscreen();
        let id = actions.id;
        on_cleanup(move || {
            // Leaving fullscreen set behind would strand a hidden bar on
            // whatever route this one popped back to.
            bar_fullscreen.set(false);
            // …and, for the identical reason, a hidden OS status/nav bar:
            // every exit that does not go through `toggle_fullscreen`'s own
            // collapse branch (a back-nav pop, Detach's landing on
            // `/sessions`, a session-guard bounce/disconnect elsewhere) is an
            // unmount, and this is the one choke point all of them share —
            // see the module docs' "OS system UI" section. Unconditional,
            // like the bar restore above: re-requesting an already-restored
            // mode costs a shell watcher a no-op poll, not a visible flicker.
            restore_system_ui();
            TERMINAL_CHROME.with(|cell| {
                let mut published = cell.borrow_mut();
                if published.as_ref().is_some_and(|actions| actions.id == id) {
                    *published = None;
                }
            });
        });

        TerminalState {
            actions,
            live: None,
        }
    }

    fn build(&self, state: &mut TerminalState) -> AnyView<TerminalState> {
        // Owned (not `&state.actions.controller`): the live-pane entry below
        // needs `&mut state` at the same time this is read from, and an
        // `Arc` clone is cheap (one refcount bump) next to a borrow-checker
        // fight.
        let controller = Arc::clone(&state.actions.controller);

        // Pane-content switch (see the module docs): a tracked read of
        // `live`, so whichever of its three entries flipped it wakes this
        // rebuild. `LiveSession` is constructed exactly once per mount,
        // guarded by `state.live.is_none()` — see `LiveSession::enter`'s own
        // doc on why this must happen here and not from a press handler.
        if controller.live.get() && state.live.is_none() {
            let session = LiveSession::enter(&controller, &state.actions.connection);
            *state.actions.live_probe.borrow_mut() = Some(LiveProbe {
                emulator: Rc::clone(&session.emulator),
                feed: session.feed.demo().map(Rc::clone),
            });
            // Published for the sheet, which switches tabs from the other side
            // of the component boundary — see `Actions::reattach`.
            *state.actions.reattach.borrow_mut() = session.reattach.clone();
            state.live = Some(session);
        }
        // Resize propagation: drain whatever the *previous* layout recorded
        // (a rotation, a font-size change, the fullscreen transition) and
        // forward it to the feed — the app-side stand-in for a real
        // `SIGWINCH`. Unthrottled per the incumbent: no debounce, and a
        // same-size layout leaves `take_pending_resize` at `None` (the
        // emulator's own no-op-at-an-unchanged-size rule), so this only ever
        // fires on an actual cell-grid delta.
        if let Some(session) = state.live.as_ref()
            && let Some((cols, rows)) = session.emulator.borrow_mut().take_pending_resize()
        {
            session.resize(cols, rows);
        }

        // Tracked reads: a load, a tab switch, a fullscreen toggle, a link
        // change or a credential turning read-only is what wakes this rebuild.
        let lines = controller.lines.get();
        let epoch = controller.reveal_epoch.get();
        let fullscreen = controller.fullscreen.get();
        let tabs = controller.tabs.get();
        let active = controller.active_tab.get();
        let title = active_title(&tabs, &active);
        let link = controller.link.get();
        // Gates every control a read-only credential would be refused for (see
        // `Actions::open_rename_dialog` and `Actions::sheet_body`), and puts
        // the grid itself in its own read-only mode, where no byte leaves the
        // session however it was derived.
        let read_only = state.actions.connection.is_read_only.get();

        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);

        // Responsive width-clamp pass (FINDINGS #21): read metrics once,
        // branch on one page constant, never touch layout again — the
        // reference idiom `examples/glyph-catalog/src/pages/responsive.rs`
        // documents. `None` (no shell has published yet, or a bare test
        // harness) takes the same branch as a genuinely narrow window.
        // Fullscreen keeps the card edge-to-edge regardless of width — see
        // `WIDE_WRAP_PAD_PX`'s doc.
        //
        // Both non-fullscreen branches add their padding *outside* the stage:
        // a wide window's all-round step-up, a narrow one's top-only
        // bar-to-card gap (`PORTRAIT_TOP_GAP_PX`, whose doc records why the
        // gap lives here rather than in the stage's own clearance).
        // Fullscreen's own zero is spelled as insets rather than as *no*
        // wrapper: the wrapper has to be in the tree in **every** state, since
        // a `Padding` that comes and goes reconciles as a different child and
        // tears the stage — the fullscreen transition's own driver included —
        // down mid-flight, snapping the animation this screen exists to play.
        let metrics = use_context::<WindowMetrics>();
        let is_wide = metrics
            .map(|m| m.size.width >= WIDE_BREAKPOINT_PX)
            .unwrap_or(false);
        let extra_wrap_pad = match (fullscreen, is_wide) {
            (true, _) => EdgeInsets::all(0.0),
            (false, true) => EdgeInsets::all(WIDE_WRAP_PAD_PX - WRAP_PAD),
            (false, false) => EdgeInsets {
                left: 0.0,
                top: PORTRAIT_TOP_GAP_PX,
                right: 0.0,
                bottom: 0.0,
            },
        };

        // The card's body: the mock transcript by default (untouched, and
        // what every existing route/navigation still reaches), or the live
        // grid once a demo session has been entered — see the module docs'
        // "Pane-content switch" note.
        let card_body: AnyView<TerminalState> = match state.live.as_ref() {
            Some(session) => {
                // Tracked: the pump's one write per wake is what repaints the
                // live pane (`term`'s own module docs on the 30 Hz mechanism).
                let generation = session.generation.get();
                let grid = term::terminal_grid(
                    Rc::clone(&session.emulator),
                    generation,
                    term::terminal_palette(&theme),
                    controller.font_size.get(),
                )
                .cursor(controller.cursor_style.get())
                // The grid's accessible name is "the session/pane it shows"
                // (`term::terminal_grid`'s own contract) — the very title the
                // status row above it prints, so a screen reader names the two
                // the same way. Empty until the first load lands, which the
                // widget reads as "no name" rather than announcing a blank one.
                .label(title.clone())
                .input(session.input.clone())
                .read_only(read_only)
                .selection(session.selection.clone());
                let mut chip = term::copy_chip(session.selection.clone());
                if let Some(sink) = demo_copy_sink() {
                    chip = chip.copy_sink(sink);
                }
                any(term::terminal_surface(
                    session.selection.clone(),
                    grid,
                    chip,
                ))
            }
            None => pieces::card_body(lines, epoch, &colors),
        };

        // The link strip sits between the status row and the body, and only
        // when there is something to say: a healthy (or absent) link adds no
        // child at all, so the card every mock-era test pins is byte for byte
        // the tree it always was.
        let mut card_children = vec![inflexible(pieces::status_row(
            &title,
            fullscreen,
            &colors,
            toggle_fullscreen,
        ))];
        if let Some(strip) = link_strip(link, &colors) {
            card_children.push(inflexible(strip));
        }
        card_children.push(flexible(1, card_body));

        let card =
            FlexView::new(Axis::Vertical, card_children).cross_axis(CrossAxisAlignment::Stretch);

        let stage = super::chrome::terminal_stage(
            card,
            fullscreen,
            colors.card,
            colors.card_border,
            Rc::clone(&state.actions.anchor),
        );
        // The card itself stays full-bleed (no clamp) at any width — only the
        // wrap padding around it grows. Wrapped from the outside rather than
        // widening `terminal_stage`'s own animated padding, which fullscreen
        // must still animate to exactly zero regardless of window width.
        let stage: AnyView<TerminalState> = any(Padding(extra_wrap_pad, stage));

        // The page tone stays **outside** the stage so it fills the whole
        // surface, bar band included: the bar slides in over it, and a page
        // that stopped at the clearance would flash the shell's own clear
        // colour through the gap for the length of that entrance.
        let page: AnyView<TerminalState> = any(term_box(stage, colors.void, 0.0));

        // Key bar mount: a fixed slot beneath the card, live-mode only —
        // the mock stays bar-free exactly as before, which is why the `None`
        // arm returns `page` completely unwrapped rather than adding an
        // always-zero-height sibling to a tree every one of
        // `tests/terminal.rs`'s ~34 tests already pins.
        match state.live.as_ref() {
            Some(session) => {
                // The keyboard's own reported height is the app's one proxy
                // for "the IME is up" — v2 has no other text input on this
                // screen to source a dedicated flag from (the module docs'
                // own note on why this mirrors `keybar`'s internal read
                // rather than inventing a second one).
                let keyboard_up = use_context::<WindowInsets>()
                    .map(|insets| insets.view_insets.bottom > 0.0)
                    .unwrap_or(false);
                any(FlexView::new(
                    Axis::Vertical,
                    vec![
                        flexible(1, page),
                        inflexible(keybar::key_bar(keyboard_up, &session.input)),
                    ],
                )
                .cross_axis(CrossAxisAlignment::Stretch))
            }
            None => page,
        }
    }
}

/// The pane title of the tab keyed `active` — the card's status row.
fn active_title(tabs: &[TabEntry], active: &str) -> String {
    tabs.iter()
        .find(|tab| tab.key == active)
        .map(|tab| tab.pane_title.clone())
        .unwrap_or_default()
}

/// The expand button's press, as the screen-state shim its callback needs.
fn toggle_fullscreen(state: &mut TerminalState) {
    state.actions.toggle_fullscreen();
}

/// Give the OS status/navigation bars back
/// ([`SystemUiMode::EdgeToEdge`], the platform default) — the one restore
/// helper every fullscreen exit path calls (see the module docs' "OS system
/// UI" section): [`Actions::toggle_fullscreen`]'s own collapse branch, and
/// `init`'s [`on_cleanup`], which is what every *other* exit funnels through
/// since each of them unmounts this screen rather than calling
/// `toggle_fullscreen` directly.
fn restore_system_ui() {
    set_system_ui_mode(SystemUiMode::EdgeToEdge);
}

/// The card's link strip for `link`, or `None` when there is nothing to say.
///
/// Four of the feed's five states are silent: `Idle` and `Connected` are a
/// session behaving, and a screen with no live `muxrd` session behind it
/// (`None` — the mock transcript, the demo replay) has no link at all. The
/// three that show speak in the same order of severity the affordance beside
/// them does: reconnecting says only that (the session is coming back on its
/// own, and a button would invite interrupting it), a disconnected one offers
/// the retry that re-arms it, and a session the server *ended* offers the only
/// thing left — the way back to the list.
///
/// Every callback here takes `&mut TerminalState`, which is exactly why the
/// strip lives in the card rather than in the sheet: `retry` needs the live
/// session, and the live session is screen state.
fn link_strip(link: Option<FeedStatus>, colors: &Palette) -> Option<AnyView<TerminalState>> {
    let (message, action): (&str, Option<AnyView<TerminalState>>) = match link? {
        FeedStatus::Idle | FeedStatus::Connected => return None,
        FeedStatus::Reconnecting => (LINK_RECONNECTING, None),
        FeedStatus::Disconnected => (
            LINK_DISCONNECTED,
            Some(any(button(LINK_RETRY, |state: &mut TerminalState| {
                if let Some(session) = state.live.as_ref() {
                    session.retry();
                }
            }))),
        ),
        FeedStatus::Closed => (
            LINK_CLOSED,
            Some(any(button(LINK_LEAVE, |state: &mut TerminalState| {
                state.actions.app_nav.pop();
            }))),
        ),
    };
    // Resolved only once there is a strip to build — a `build` that has
    // nothing to say must stay as cheap as it was before this existed.
    let mut style = pieces::active_theme().type_scale.label_medium.clone();
    style.color = colors.fg_muted;
    Some(status_strip(message, style, colors.head, action))
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

impl Actions {
    /// Flip fullscreen: the card animates itself off the controller's flag,
    /// the **shell's bar** is told to animate away through its own existing
    /// hidden/reveal driver, and the **OS** status/nav bars follow suit —
    /// see the module docs' "OS system UI" section for why this is a
    /// one-shot call here rather than a `build`-time read.
    pub fn toggle_fullscreen(&self) {
        let on = self.controller.toggle_fullscreen();
        self.app_nav.set_fullscreen(on);
        if on {
            // Immersive-sticky, not plain immersive: a swipe from the edge
            // reveals the bars transiently and they auto-hide again, rather
            // than staying shown until a second explicit action — the
            // right feel for a terminal a user dips in and out of
            // (`examples/shadertoy`'s own `ImmersiveSticky` call is the same
            // choice for its fullscreen shader view).
            set_system_ui_mode(SystemUiMode::ImmersiveSticky);
        } else {
            restore_system_ui();
        }
    }

    /// Open the app bar's overflow menu, anchored to the bar slot the `⋮`
    /// actually paints in.
    ///
    /// `show_glyph_menu` takes a **caller-reported** window-coordinate rect
    /// (there is no ancestor-bounds query a widget can make), and the slot it
    /// anchors to belongs to `crate::ui::topbar` — a sibling this page cannot
    /// reach into. So `super::chrome::terminal_stage`, the one widget that
    /// already knows both the window box and the status-bar inset, computes
    /// that rect during layout and drops it in [`anchor`](Self::anchor); this
    /// reads it back. Same shape as `examples/glyph-catalog`'s own
    /// `AnchorReporter`, minus the wrapper.
    ///
    /// The menu is a transparent push onto the **root overlay host**, so it
    /// paints over the bar it is anchored to (rather than clipping under it),
    /// and the bar goes inert for free — the host routes input to its top page
    /// only, which is also what makes a tap anywhere outside the panel (the
    /// bar included) dismiss it. Unlike a sheet or a dialog the menu paints no
    /// scrim of its own, by `frust::glyph::menu`'s own design: a menu is not a
    /// dimming modal, so nothing under it dims — the bar least of all.
    ///
    /// The anchor rect is unaffected by the move: it is reported in **window**
    /// coordinates, and the host's root page spans the whole window exactly as
    /// the route navigator did.
    ///
    /// **The menu claims root focus on every press, and this app cannot stop
    /// it** — a FINDINGS candidate, not a bug on this side. `frust::glyph`'s
    /// menu widget requests focus on *any* pointer `Down` ("a press anywhere
    /// claims focus so a subsequent Escape has a chain to travel"), and
    /// [`show_glyph_menu`] exposes no seam to opt out. So the very `Down` that
    /// picks a row seeds a root focus session with no IME surface behind it
    /// until the menu's exit and pop release it. It
    /// summons no keyboard (nothing publishes a surface, and the grid is not on
    /// the focus path), and on a shell whose frame gate reads the focus/IME
    /// **edge** it costs two frames rather than a steady wake; the same press
    /// pattern lives in `crate::ui::sheet`'s own `ModalStage`, whose claim
    /// instead lasts as long as the sheet is up. `tests/terminal_live.rs`'s
    /// section 10 pins what this side controls: mounting the live pane claims
    /// nothing, the transient claim leaves with the menu, and only a tap on the
    /// grid opens a real session.
    pub fn open_menu(&self) {
        let actions = self.clone();
        show_glyph_menu(
            &self.overlays,
            self.anchor.get(),
            menu_entries,
            move |_app: &mut AppState, result: PopResult| {
                match result.take::<usize>() {
                    Some(MENU_PANES_INDEX) => actions.open_sheet(),
                    Some(MENU_RENAME_INDEX) => actions.open_rename_dialog(),
                    Some(MENU_DETACH_INDEX) => actions.detach(),
                    // Every other dismissal (scrim, Escape, back) pops empty.
                    _ => {}
                }
            },
        );
    }

    /// Open the "panes & tabs" sheet — the menu's first row.
    ///
    /// The body builder is re-invoked on every rebuild while the sheet is up
    /// (the navigator's page-builder contract), so its reads are tracked and a
    /// browse is reflected in the pills, in the section label *and* in the pane
    /// list without the sheet being rebuilt by hand.
    ///
    /// Opening always points the browse back at the tab the session is on
    /// ([`TerminalController::begin_browsing`]) — a browse is a look around,
    /// not a place the sheet remembers being.
    pub fn open_sheet(&self) {
        self.controller.begin_browsing();
        let actions = self.clone();
        show_sheet(&self.overlays, move |sheet| actions.sheet_body(sheet));
    }

    /// Point the sheet at the tab keyed `key`.
    ///
    /// Local, synchronous and RPC-free: it re-slices the pane list below out of
    /// the tab world the load already carried (see
    /// [`TerminalController::browse_tab`]). Nothing about the session moves,
    /// and the sheet stays up — only [`commit_pane`](Self::commit_pane) does
    /// either of those.
    pub fn browse_tab(&self, key: &str) {
        self.controller.browse_tab(key.to_string());
    }

    /// Commit the sheet's browse: focus the pane `pane_id` of the tab keyed
    /// `tab_key`, then close the sheet a beat later so the selection visibly
    /// moves first (`setTimeout(closeSheet, 260)`).
    ///
    /// The sheet's **one** navigating gesture, and its one dismissal: it stays
    /// live for the whole close beat, so the tap can be repeated — and it is
    /// [`SheetHandle::close_after`] that makes repeating it safe. **Latest tap
    /// wins**: a second row re-times the one pending close off its own
    /// selection rather than stacking a second close behind the first, and the
    /// gate underneath means the superseded wait cannot pop the `/terminal`
    /// route on its way out.
    ///
    /// A commit that has to move **tabs** first is refused for a read-only
    /// credential rather than by the server: the wire's `GoToTab` is MUTATING
    /// (`proto/muxr.proto`), so the row says why — the same
    /// "the control stays, the tap does nothing" language
    /// [`open_rename_dialog`](Self::open_rename_dialog) uses. Committing a pane
    /// *inside* the tab the session is already on is `FocusPane` alone, which
    /// the wire allows a read-only token, so it stays available.
    ///
    /// On a backend whose stream carries only the focused pane, the move is not
    /// done when the RPCs land: the open stream keeps replaying the pane it
    /// attached to, so the session has to be attached again to start receiving
    /// the new one. [`TerminalController::reattaches_on_switch`] answers whether
    /// that applies, and [`reattach`](Self::reattach) is how this side of the
    /// component boundary reaches a session it cannot see.
    pub fn commit_pane(&self, sheet: &SheetHandle, tab_key: &str, pane_id: u32, is_plugin: bool) {
        // Untracked: a press handler has no reactive owner on desktop.
        let cross_tab = tab_key != self.controller.active_tab.get_untracked();
        if cross_tab && self.connection.is_read_only.get_untracked() {
            app_toasts().show(READ_ONLY_TOAST);
            return;
        }
        let controller = Arc::clone(&self.controller);
        let reattach = Rc::clone(&self.reattach);
        let sheet = sheet.clone();
        let tab_key = tab_key.to_string();
        spawn_local(async move {
            if !controller.commit_pane(tab_key, pane_id, is_plugin).await {
                // A refused commit leaves the sheet up: nothing moved, and the
                // failure toast is already on its way.
                return;
            }
            if controller.reattaches_on_switch() {
                // Cloned out of the cell first: the hook re-enters this screen's
                // own state, and a borrow held across it would be a second one.
                let hook = reattach.borrow().clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            sheet.close_after(SHEET_CLOSE_DELAY).await;
        });
    }

    /// Switch this connection's view to the space with id `id`, reloading the
    /// tab world behind it.
    ///
    /// Stays available to a read-only credential: which space a client is
    /// looking at is per-connection state, not a change to the session (see
    /// [`TerminalRepository::switch_space`](crate::features::terminal::domain::repositories::TerminalRepository::switch_space)).
    /// The sheet stays up — a space is a place to browse from, not a
    /// destination.
    pub fn select_space(&self, id: &str) {
        let controller = Arc::clone(&self.controller);
        let id = id.to_string();
        spawn_local(async move { controller.select_space(id).await });
    }

    /// Open the rename dialog, seeded with the attached session's *current*
    /// name — re-read from the controller rather than captured, so a menu
    /// opened several frames ago cannot seed a stale name.
    ///
    /// With no attached session there is nothing to rename, and the dialog
    /// does not open (the `features::sessions` precedent for a row whose
    /// target has gone).
    ///
    /// A rename is **mutating**, so a read-only credential is refused here
    /// rather than at the server: `frust::glyph::MenuEntry` carries no
    /// disabled state (it is a label plus a normal/danger variant), so the row
    /// stays on screen and says why — the same "the row stays, the tap does
    /// nothing" language `features::sessions`' own management sheet uses for a
    /// read-only session, minus the greying it can express and this menu
    /// cannot.
    pub fn open_rename_dialog(&self) {
        // Untracked throughout: a press handler has no reactive owner on
        // desktop, and these reads must never make one.
        if self.connection.is_read_only.get_untracked() {
            app_toasts().show(READ_ONLY_TOAST);
            return;
        }
        let Some(current) = self.controller.session_name.get_untracked() else {
            return;
        };
        let controller = Arc::clone(&self.controller);
        name_input_dialog(
            &self.overlays,
            RENAME_TITLE,
            current,
            move |_app: &mut AppState, name: String| {
                let handle = Arc::clone(&controller);
                spawn_local(async move {
                    if let Some(name) = handle.rename(name).await {
                        app_toasts().show(format!("renamed to {name}"));
                    }
                });
            },
        );
    }

    /// Detach from the attached session: pop back to the list, then say so.
    ///
    /// **One pop per mount**, latched through [`Actions::detaching`] — the
    /// same single-shot rule the sheet's own dismissal follows
    /// ([`SheetHandle::close`]), and for the same reason: the detach is async,
    /// the menu can be reopened while it is in flight, and a second landing
    /// pop would take `/sessions` off the stack too rather than merely
    /// arriving there twice. A detach the repository *refuses* re-arms the
    /// latch, since nothing moved and the user is entitled to try again.
    pub fn detach(&self) {
        if self.detaching.replace(true) {
            return;
        }
        let controller = Arc::clone(&self.controller);
        let nav = self.app_nav.clone();
        let detaching = Rc::clone(&self.detaching);
        spawn_local(async move {
            if controller.detach().await {
                nav.pop();
                app_toasts().show(DETACHED_TOAST);
            } else {
                detaching.set(false);
            }
        });
    }

    /// What the sheet's **create** affordances say when tapped: `+` and `+ new
    /// pane` — plus `+ space` on a backend that has spaces at all — are drawn
    /// and unwired, and this is how they say so rather than appearing to work.
    ///
    /// A read-only credential withdraws them from the sheet outright (see
    /// [`sheet_body`](Self::sheet_body)) — offering a create nobody may
    /// perform is worse than not offering one.
    pub fn unavailable(&self) {
        app_toasts().show(UNAVAILABLE_TOAST);
    }

    // -- the sheet ---------------------------------------------------------

    /// The "panes & tabs" sheet's body — `docs/terminal.html`'s
    /// `#pickerSheet`, top to bottom, minus the handle the scaffold already
    /// draws.
    ///
    /// **Browse, then commit.** A tab pill points the sheet at another tab and
    /// re-slices the pane list below it (no RPC, no dismissal); a space pill
    /// switches this connection's view and reloads behind it (still no
    /// dismissal); only a pane row moves the session and closes the sheet. See
    /// [`browse_tab`](Self::browse_tab)/[`select_space`](Self::select_space)/
    /// [`commit_pane`](Self::commit_pane).
    ///
    /// **The spaces section is herdr's alone** — zellij has no spaces at all
    /// (see [`TerminalController::has_spaces`]), so the section is *absent*
    /// rather than empty: no label, and no gap where one would have been.
    fn sheet_body(&self, sheet: &SheetHandle) -> AnyView<AppState> {
        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);
        // Tracked reads: browsing a tab, committing a pane or switching a
        // space from inside the sheet re-runs this builder, and a credential
        // turning read-only withdraws the three create affordances below.
        let spaces = self.controller.spaces.get();
        let tabs = self.controller.tabs.get();
        let active = self.controller.active_tab.get();
        let browsed = self.controller.browsed_tab.get();
        let read_only = self.connection.is_read_only.get();

        let mut children = vec![self.top_row(sheet, &colors), pieces::v_gap(SECTION_TOP_GAP)];

        if self.controller.has_spaces() {
            children.push(pieces::section_label(SECTION_SPACES, &colors));
            children.push(pieces::v_gap(SECTION_BOTTOM_GAP));
            let mut space_pills: Vec<AnyView<AppState>> = spaces
                .iter()
                .map(|space| {
                    let actions = self.clone();
                    let id = space.id.clone();
                    pieces::pill(
                        space.name.clone(),
                        if space.selected {
                            PillState::Selected
                        } else {
                            PillState::Idle
                        },
                        &colors,
                        move |_app: &mut AppState| actions.select_space(&id),
                    )
                })
                .collect();
            if !read_only {
                space_pills.push(self.add_pill(ADD_SPACE, &colors));
            }
            children.push(super::chrome::wrap_row(space_pills, PILL_GAP));
            // The section's own trailing gap, so the tabs label below sits the
            // same distance from whatever precedes it either way.
            children.push(pieces::v_gap(SECTION_TOP_GAP));
        }

        children.push(pieces::section_label(SECTION_TABS, &colors));
        children.push(pieces::v_gap(SECTION_BOTTOM_GAP));
        let mut tab_pills: Vec<AnyView<AppState>> = tabs
            .iter()
            .map(|tab| {
                let actions = self.clone();
                let key = tab.key.clone();
                // The browsed pill wears the full selection; the tab the
                // session is actually on keeps an amber edge behind it, so a
                // browse never hides where the terminal is.
                let state = if tab.key == browsed {
                    PillState::Selected
                } else if tab.key == active {
                    PillState::Active
                } else {
                    PillState::Idle
                };
                pieces::pill(
                    tab.pill_label(),
                    state,
                    &colors,
                    move |_app: &mut AppState| actions.browse_tab(&key),
                )
            })
            .collect();
        if !read_only {
            tab_pills.push(self.add_pill(ADD_TAB, &colors));
        }
        children.push(super::chrome::wrap_row(tab_pills, PILL_GAP));

        children.push(pieces::v_gap(SECTION_TOP_GAP));
        children.push(pieces::section_label(
            format!("{SECTION_PANES}{browsed}"),
            &colors,
        ));
        children.push(pieces::v_gap(SECTION_BOTTOM_GAP));
        if let Some(tab) = tabs.iter().find(|tab| tab.key == browsed) {
            // A tab the sheet is only *browsing* shows no badge at all: its
            // panes carry whatever the last load recorded, and the session is
            // not in any of them.
            let on_session = tab.key == active;
            for pane in &tab.panes {
                let actions = self.clone();
                let sheet = sheet.clone();
                let key = tab.key.clone();
                let (id, is_plugin) = (pane.id, pane.is_plugin);
                children.push(pieces::pane_item(
                    pane,
                    on_session && pane.focused,
                    &colors,
                    move |_app: &mut AppState| actions.commit_pane(&sheet, &key, id, is_plugin),
                ));
                children.push(pieces::v_gap(PANE_ROW_GAP));
            }
        }
        if !read_only {
            let actions = self.clone();
            children.push(pieces::new_pane_row(&colors, move |_app: &mut AppState| {
                actions.unavailable()
            }));
        }

        any(pieces::column(children))
    }

    /// The sheet's top row, whose close button pops it — through the same one
    /// gate a pill's delayed close uses, so pressing it mid-beat closes the
    /// sheet once and leaves the route alone.
    fn top_row(&self, sheet: &SheetHandle, colors: &Palette) -> AnyView<AppState> {
        let sheet = sheet.clone();
        pieces::sheet_top_row(colors, move |_app: &mut AppState| {
            sheet.close();
        })
    }

    /// One `.pill.add` — dashed, dim, and (this phase) a toast.
    fn add_pill(&self, label: &'static str, colors: &Palette) -> AnyView<AppState> {
        let actions = self.clone();
        pieces::pill(label, PillState::Add, colors, move |_app: &mut AppState| {
            actions.unavailable()
        })
    }
}

/// The overflow menu's entries. A free fn because `show_glyph_menu` takes a
/// `Fn() -> Vec<MenuEntry>` it may re-invoke per rebuild.
///
/// **No icons**: `frust::glyph::MenuEntry` is a label plus a
/// normal/danger variant, with no icon slot at all, while the Glyph design
/// system's own overflow menu (and this mock) draws one per row. Recorded as a
/// FINDINGS candidate rather than worked around — setting `▤`/`✎`/`⏻` as text
/// in the label is exactly the font-fallback trap `crate::ui::topbar`'s
/// vector rule forbids, and there is no other seam to reach.
///
/// *Detach* is deliberately **not** `menu_item_danger`: detaching leaves the
/// session running (that is the whole difference from the sessions screen's
/// Kill), so painting it in the error role would misdescribe it.
///
/// **No separators either.** A separator is grouping punctuation, and three
/// rows have no groups to punctuate — one carried an 11px band of dead space
/// (`frust::glyph::menu`'s own `SEP_TOTAL_H`) that read on device as the rows
/// being unevenly spaced. The demo-session row that used to sit behind the
/// second separator is gone with it (see the [module docs](self)'s
/// "Pane-content switch").
fn menu_entries() -> Vec<MenuEntry> {
    vec![
        menu_item(MENU_PANES),
        menu_item(MENU_RENAME),
        menu_item(MENU_DETACH),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_is_three_uniformly_spaced_rows_and_nothing_else() {
        // The whole shape, in one assertion: no demo row, and no separator to
        // put an 11px gap between two of the three that are left.
        let entries = menu_entries();
        assert_eq!(entries.len(), 3, "three rows, no `.dd-sep`s");
        assert!(
            entries
                .iter()
                .all(|entry| matches!(entry, MenuEntry::Item(_))),
            "a separator would space the rows unevenly"
        );
        let labels: Vec<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                MenuEntry::Item(item) => Some(item.label.clone()),
                MenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(labels, [MENU_PANES, MENU_RENAME, MENU_DETACH]);
    }

    #[test]
    fn the_named_menu_indices_address_the_rows_they_claim_to() {
        // The indices the result callback matches on are into the *entry*
        // list — an off-by-one here would silently run the wrong action.
        let entries = menu_entries();
        for (index, label) in [
            (MENU_PANES_INDEX, MENU_PANES),
            (MENU_RENAME_INDEX, MENU_RENAME),
            (MENU_DETACH_INDEX, MENU_DETACH),
        ] {
            match &entries[index] {
                MenuEntry::Item(item) => assert_eq!(item.label, label),
                MenuEntry::Separator => panic!("{label} landed on a separator"),
            }
        }
    }

    #[test]
    fn the_active_title_is_the_active_tabs_pane_title() {
        let tab = |key: &str, title: &str| TabEntry {
            index: 1,
            tab_id: 1,
            key: key.to_string(),
            label: key.to_string(),
            pane_title: title.to_string(),
            pane_name: String::new(),
            pane_path: String::new(),
            panes: Vec::new(),
            active: false,
        };
        let tabs = vec![
            tab("btop", "ed@precision: ~"),
            tab("devbox", "ed@devbox: ~"),
        ];
        assert_eq!(active_title(&tabs, "devbox"), "ed@devbox: ~");
        assert_eq!(active_title(&tabs, "btop"), "ed@precision: ~");
        assert_eq!(active_title(&tabs, "gone"), "", "no tab, no title");
    }

    #[test]
    fn the_sheet_closes_a_beat_after_a_tab_switch_rather_than_instantly() {
        // The mock's own 260ms: long enough to see the pill move, short enough
        // not to feel stuck.
        assert_eq!(SHEET_CLOSE_DELAY.as_millis(), 260);
    }
}
