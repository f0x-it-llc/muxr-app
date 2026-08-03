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
//! # Pane-content switch: the mock stays default, "Demo session" opts in
//!
//! The card's body renders [`pieces::card_body`] (the seeded mock transcript)
//! for every existing route and navigation — untouched, and pinned by every
//! one of `tests/terminal.rs`'s ~34 tests, which assert that exact content
//! through this real composition root. The live [`term::terminal_surface`] is
//! reached only by tapping the overflow menu's **`Demo session`** row
//! ([`MENU_DEMO_INDEX`]), which flips [`TerminalController::live`] — the one
//! flag [`TerminalScreen::build`] branches the card's body (and the key bar's
//! mount) on.
//!
//! **Decision (deviates from the plan's literal suggestion):** the plan's
//! default was a seeded "demo" row in the mock *session* list
//! (`data::repositories::mock_terminal_repository`). Reading that
//! repository's actual model, its `spaces`/`tabs` pill rows are exactly what
//! `tests/terminal.rs`'s `the_menus_first_row_opens_the_sheet_with_every_pill_row_and_the_pane_item`
//! pins by **count** (`3 + 1 + 4 + 1` pills) — any seeded row in either list
//! changes that count and breaks an unmodifiable test. The overflow menu
//! already carries exactly this shape of one-off screen action
//! (`Panes & tabs`/`Rename session`/`Detach session`), asserted only by
//! *label*, never by a total row count in `tests/terminal.rs` — so a fourth
//! row is the cleaner, v2-consistent entry the module docs on this file's own
//! task invited: additive, unobtrusive (one more overflow-menu row, the same
//! shape as the other three), and public-safe (`Demo session` names nothing
//! internal).
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
use frust::glyph::{MenuEntry, menu_item, menu_separator, show_glyph_menu};
use frust::{
    AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Get, GetUntracked,
    NavigatorController, Padding, PopResult, WindowMetrics, any, component, flexible, inflexible,
    on_cleanup, spawn_local, use_context,
};

use crate::AppState;
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::settings::di::expect_preferences_repository;
use crate::features::terminal::data::feeds::demo_feed::DemoFeed;
use crate::features::terminal::di::expect_terminal_repository;
use crate::features::terminal::domain::entities::TabEntry;
use crate::features::terminal::domain::feed::TerminalFeed;
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::name_input_dialog;
use crate::ui::sheet::{SheetHandle, show_sheet};
use crate::ui::toasts::app_toasts;

use super::chrome::{WRAP_PAD, term_box};
use super::controller::{LiveSession, TerminalController};
use super::keybar;
use super::pieces::{
    self, PANE_ROW_GAP, PILL_GAP, Palette, PillState, SECTION_BOTTOM_GAP, SECTION_TOP_GAP,
};
use super::term;

/// How long the sheet stays up after a tab pill is tapped, so the selection
/// visibly moves before it closes — `setTimeout(closeSheet, 260)` in the mock.
const SHEET_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(260);

/// The overflow menu's rows, in order. Indices into the entry list
/// [`show_glyph_menu`] hands back, so they are named once rather than spelled
/// at the match site.
const MENU_PANES: &str = "Panes & tabs";
const MENU_RENAME: &str = "Rename session";
const MENU_DETACH: &str = "Detach session";
/// The demo-session entry — see the [module docs](self)'s "Demo entry"
/// note. Appended after its own separator rather than woven into the first
/// three rows, so `MENU_RENAME_INDEX`/`MENU_DETACH_INDEX` (and every existing
/// index-based test in `tests/terminal.rs`) stay exactly where they were.
const MENU_DEMO: &str = "Demo session";
const MENU_PANES_INDEX: usize = 0;
const MENU_RENAME_INDEX: usize = 2;
const MENU_DETACH_INDEX: usize = 3;
const MENU_DEMO_INDEX: usize = 5;

/// The rename dialog's title.
const RENAME_TITLE: &str = "Rename session";
/// What the two UI-only affordances (the add pills, the new-pane row) say.
const UNAVAILABLE_TOAST: &str = "not yet available";
/// …and what a landed detach says on the way back to the list.
const DETACHED_TOAST: &str = "detached";

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

const _: () = {
    assert!(WIDE_WRAP_PAD_PX > WRAP_PAD);
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
    pub feed: Rc<RefCell<DemoFeed>>,
}

/// The mounted screen's live demo pane, if one has been entered — see
/// [`LiveProbe`]. `None` before "Demo session" is chosen, or when no terminal
/// screen is mounted at all.
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

/// Open the terminal's overflow menu — what `crate::shell` forwards the app
/// bar's `⋮` press to.
///
/// The feature owns its own menu; the shell only forwards (the same split the
/// `⚙`'s route push follows). A press that arrives with no terminal screen
/// mounted is silently ignored: the bar only ever carries this action on
/// `/terminal`, so that is an unreachable race rather than a state to handle.
/// Test/diagnostic accessor: the mounted screen's controller, if any — lets a
/// headless test read `TerminalController::cursor_style`/`font_size`/`live`
/// (e.g. the settings-reflect-into-the-live-pane acceptance test) through the
/// same [`TERMINAL_CHROME`] handle [`open_terminal_menu`] already reads.
pub fn debug_controller() -> Option<Arc<TerminalController>> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|actions| Arc::clone(&actions.controller))
    })
}

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

        // First load: spaces, tabs, the active tab's lines, the session name.
        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.load().await });

        let actions = Actions {
            id: NEXT_CHROME_ID.fetch_add(1, Ordering::Relaxed),
            overlays: self.overlays.clone(),
            app_nav,
            controller,
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

        // Pane-content switch (see the module docs' "Demo entry" note): a
        // tracked read of `live`, so flipping it (the overflow menu's "Demo
        // session" row) wakes this rebuild. `LiveSession` is constructed
        // exactly once per mount, guarded by `state.live.is_none()` — see
        // `LiveSession::enter`'s own doc on why this must happen here and not
        // from the menu's press handler.
        if controller.live.get() && state.live.is_none() {
            let session = LiveSession::enter(&controller);
            *state.actions.live_probe.borrow_mut() = Some(LiveProbe {
                emulator: Rc::clone(&session.emulator),
                feed: Rc::clone(&session.feed),
            });
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
            session.feed.borrow_mut().resize(cols, rows);
        }

        // Tracked reads: a load, a tab switch or a fullscreen toggle is what
        // wakes this rebuild.
        let lines = controller.lines.get();
        let epoch = controller.reveal_epoch.get();
        let fullscreen = controller.fullscreen.get();
        let tabs = controller.tabs.get();
        let active = controller.active_tab.get();
        let title = active_title(&tabs, &active);

        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);

        // Responsive width-clamp pass (FINDINGS #21): read metrics once,
        // branch on one page constant, never touch layout again — the
        // reference idiom `examples/glyph-catalog/src/pages/responsive.rs`
        // documents. `None` (no shell has published yet, or a bare test
        // harness) takes the same branch as a genuinely narrow window.
        // Fullscreen keeps the card edge-to-edge regardless of width — see
        // `WIDE_WRAP_PAD_PX`'s doc.
        let metrics = use_context::<WindowMetrics>();
        let is_wide = metrics
            .map(|m| m.size.width >= WIDE_BREAKPOINT_PX)
            .unwrap_or(false);
        let extra_wrap_pad = if is_wide && !fullscreen {
            WIDE_WRAP_PAD_PX - WRAP_PAD
        } else {
            0.0
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
                .label(MENU_DEMO)
                .input(session.input.clone())
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

        let card = FlexView::new(
            Axis::Vertical,
            vec![
                inflexible(pieces::status_row(
                    &title,
                    fullscreen,
                    &colors,
                    toggle_fullscreen,
                )),
                flexible(1, card_body),
            ],
        )
        .cross_axis(CrossAxisAlignment::Stretch);

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
        let stage: AnyView<TerminalState> = if extra_wrap_pad > 0.0 {
            any(Padding(EdgeInsets::all(extra_wrap_pad), stage))
        } else {
            any(stage)
        };

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

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

impl Actions {
    /// Flip fullscreen: the card animates itself off the controller's flag,
    /// and the **shell's bar** is told to animate away through its own
    /// existing hidden/reveal driver.
    pub fn toggle_fullscreen(&self) {
        let on = self.controller.toggle_fullscreen();
        self.app_nav.set_fullscreen(on);
    }

    /// Enter the live demo pane — the overflow menu's "Demo session" row.
    /// Only flips the controller's flag; `TerminalScreen::build` is what
    /// actually constructs the seams/feed the first time it observes it (see
    /// that method's own doc on why a press handler cannot do that itself).
    pub fn enter_demo(&self) {
        self.controller.enter_live();
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
                    Some(MENU_DEMO_INDEX) => actions.enter_demo(),
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
    /// tab switch is reflected in the pills *and* in the section label without
    /// the sheet being rebuilt by hand.
    pub fn open_sheet(&self) {
        let actions = self.clone();
        show_sheet(&self.overlays, move |sheet| actions.sheet_body(sheet));
    }

    /// Switch the card to `key` and close the sheet a beat later, so the
    /// selection visibly moves first (`setTimeout(closeSheet, 260)`).
    ///
    /// The sheet stays live for that whole beat, so the tap can be repeated —
    /// and it is [`SheetHandle::close_after`] that makes repeating it safe.
    /// **Latest tap wins**: a second pill re-times the one pending close off
    /// its own selection (the beat is there to be *seen*, and the second
    /// selection is the one the user is looking at) rather than stacking a
    /// second close behind the first, and the gate underneath means the
    /// superseded wait cannot pop the `/terminal` route on its way out.
    pub fn switch_tab(&self, sheet: &SheetHandle, key: &str) {
        let controller = Arc::clone(&self.controller);
        let sheet = sheet.clone();
        let key = key.to_string();
        spawn_local(async move {
            controller.switch_tab(key).await;
            sheet.close_after(SHEET_CLOSE_DELAY).await;
        });
    }

    /// Select a space — selection state only, in the mock era.
    pub fn select_space(&self, name: &str) {
        self.controller.select_space(name);
    }

    /// Open the rename dialog, seeded with the attached session's *current*
    /// name — re-read from the controller rather than captured, so a menu
    /// opened several frames ago cannot seed a stale name.
    ///
    /// With no attached session there is nothing to rename, and the dialog
    /// does not open (the `features::sessions` precedent for a row whose
    /// target has gone).
    pub fn open_rename_dialog(&self) {
        // Untracked: a press handler has no reactive owner on desktop, and
        // this read must never make one.
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

    /// What the UI-only affordances say when tapped (the two add pills and the
    /// new-pane row): this phase draws them, the functional phase wires them.
    pub fn unavailable(&self) {
        app_toasts().show(UNAVAILABLE_TOAST);
    }

    // -- the sheet ---------------------------------------------------------

    /// The "panes & tabs" sheet's body — `docs/terminal.html`'s
    /// `#pickerSheet`, top to bottom, minus the handle the scaffold already
    /// draws.
    fn sheet_body(&self, sheet: &SheetHandle) -> AnyView<AppState> {
        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);
        // Tracked reads: switching a tab from inside the sheet re-runs this
        // builder and moves the selection.
        let spaces = self.controller.spaces.get();
        let tabs = self.controller.tabs.get();
        let active = self.controller.active_tab.get();

        let mut children = vec![self.top_row(sheet, &colors), pieces::v_gap(SECTION_TOP_GAP)];

        children.push(pieces::section_label(SECTION_SPACES, &colors));
        children.push(pieces::v_gap(SECTION_BOTTOM_GAP));
        let mut space_pills: Vec<AnyView<AppState>> = spaces
            .iter()
            .map(|space| {
                let actions = self.clone();
                let name = space.name.clone();
                pieces::pill(
                    space.name.clone(),
                    if space.selected {
                        PillState::Selected
                    } else {
                        PillState::Idle
                    },
                    &colors,
                    move |_app: &mut AppState| actions.select_space(&name),
                )
            })
            .collect();
        space_pills.push(self.add_pill(ADD_SPACE, &colors));
        children.push(super::chrome::wrap_row(space_pills, PILL_GAP));

        children.push(pieces::v_gap(SECTION_TOP_GAP));
        children.push(pieces::section_label(SECTION_TABS, &colors));
        children.push(pieces::v_gap(SECTION_BOTTOM_GAP));
        let mut tab_pills: Vec<AnyView<AppState>> = tabs
            .iter()
            .map(|tab| {
                let actions = self.clone();
                let sheet = sheet.clone();
                let key = tab.key.clone();
                pieces::pill(
                    tab.pill_label(),
                    if tab.key == active {
                        PillState::Selected
                    } else {
                        PillState::Idle
                    },
                    &colors,
                    move |_app: &mut AppState| actions.switch_tab(&sheet, &key),
                )
            })
            .collect();
        tab_pills.push(self.add_pill(ADD_TAB, &colors));
        children.push(super::chrome::wrap_row(tab_pills, PILL_GAP));

        children.push(pieces::v_gap(SECTION_TOP_GAP));
        children.push(pieces::section_label(
            format!("{SECTION_PANES}{active}"),
            &colors,
        ));
        children.push(pieces::v_gap(SECTION_BOTTOM_GAP));
        if let Some(tab) = tabs.iter().find(|tab| tab.key == active) {
            children.push(pieces::pane_item(tab, &colors));
            children.push(pieces::v_gap(PANE_ROW_GAP));
        }
        let actions = self.clone();
        children.push(pieces::new_pane_row(&colors, move |_app: &mut AppState| {
            actions.unavailable()
        }));

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
/// *Demo session* sits behind its own separator, after the first three: it is
/// not a session-management action like the rest of the menu, so it is kept
/// visually apart from them (see the module docs' "Demo entry" note on why it
/// lives here at all rather than as a seeded pill row).
fn menu_entries() -> Vec<MenuEntry> {
    vec![
        menu_item(MENU_PANES),
        menu_separator(),
        menu_item(MENU_RENAME),
        menu_item(MENU_DETACH),
        menu_separator(),
        menu_item(MENU_DEMO),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_is_four_rows_with_a_separator_before_the_last_two_groups() {
        let entries = menu_entries();
        assert_eq!(entries.len(), 6, "four rows plus the two `.dd-sep`s");
        assert!(matches!(entries[1], MenuEntry::Separator));
        assert!(matches!(entries[4], MenuEntry::Separator));
        let labels: Vec<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                MenuEntry::Item(item) => Some(item.label.clone()),
                MenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(labels, [MENU_PANES, MENU_RENAME, MENU_DETACH, MENU_DEMO]);
    }

    #[test]
    fn the_named_menu_indices_address_the_rows_they_claim_to() {
        // The indices the result callback matches on are into the *entry*
        // list, separator included — an off-by-one here would silently run the
        // wrong action.
        let entries = menu_entries();
        for (index, label) in [
            (MENU_PANES_INDEX, MENU_PANES),
            (MENU_RENAME_INDEX, MENU_RENAME),
            (MENU_DETACH_INDEX, MENU_DETACH),
            (MENU_DEMO_INDEX, MENU_DEMO),
        ] {
            match &entries[index] {
                MenuEntry::Item(item) => assert_eq!(item.label, label),
                MenuEntry::Separator => panic!("{label} landed on the separator"),
            }
        }
    }

    #[test]
    fn the_active_title_is_the_active_tabs_pane_title() {
        let tab = |key: &str, title: &str| TabEntry {
            index: 1,
            key: key.to_string(),
            label: key.to_string(),
            pane_title: title.to_string(),
            pane_name: String::new(),
            pane_path: String::new(),
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
