//! The `/terminal` route's page: muxr's terminal surface, a pixel copy of
//! `docs/terminal.html` v2 — no tabs strip, no path bar, no cmd hint, no FAB.
//! The overflow `⋮` lives on the *app* bar (`crate::shell` forwards it to
//! [`open_terminal_menu`]), and the pane title sits in the card's own status row
//! beside the fullscreen toggle.
//!
//! [`terminal_page`] is what `crate::routes` mounts; below it is the canonical
//! screen shape — a per-screen [`Component`] whose `init` hosts a
//! [`TerminalController`] through `use_controller`, and whose `build` is pure
//! arrangement over [`super::widgets`]' elements. Every contract is documented
//! where it is enforced: fullscreen on
//! [`Actions::toggle_fullscreen`]/[`restore_system_ui`], the component-state
//! boundary on [`Actions`], the mock-vs-live switch on
//! [`TerminalScreen::build`], gesture routing on
//! [`resolve_tap_action`]/[`resolve_wheel_route`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::authoring::{Rect, WindowInsets};
use frust::{
    AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Get, GetUntracked,
    NavigatorController, Padding, PopResult, RwSignal, SystemUiMode, WindowMetrics, any, component,
    flexible, inflexible, on_cleanup, set_system_ui_mode, spawn_local, use_context,
};
use frust_glyph::{MenuEntry, menu_item, show_glyph_dialog, show_glyph_menu};

use crate::AppState;
use crate::connection::{CAP_MOUSE_INPUT, ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::settings::di::expect_preferences_repository;
use crate::features::terminal::data::feeds::demo_feed::DemoFeed;
use crate::features::terminal::di::expect_terminal_repository;
use crate::features::terminal::domain::entities::{PaneEntry, ScrollStep, TabEntry};
use crate::features::terminal::domain::feed::WheelKind;
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::name_input_dialog;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::{SheetHandle, show_sheet};
use crate::ui::toasts::app_toasts;

use super::chrome::{WRAP_PAD, term_box};
use super::controller::{LiveSession, TerminalController};
use super::keybar;
use super::term;
use super::widgets::card::{CardInk, card_body};
use super::widgets::link_strip::link_strip;
use super::widgets::new_tab_dialog::new_tab_dialog;
use super::widgets::panes_sheet::sheet_body;
use super::widgets::status::status_row;

/// How long the sheet stays up after a **pane row** is tapped, so the selection
/// visibly moves before it closes — `setTimeout(closeSheet, 260)` in the mock. A
/// pane tap is the sheet's one dismissing gesture ([`Actions::commit_pane`]).
const SHEET_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(260);

/// The overflow menu's rows, in order, plus their indices into the entry list
/// [`show_glyph_menu`] hands back.
const MENU_PANES: &str = "Panes & tabs";
const MENU_RENAME: &str = "Rename session";
const MENU_DETACH: &str = "Detach session";
const MENU_PANES_INDEX: usize = 0;
const MENU_RENAME_INDEX: usize = 1;
const MENU_DETACH_INDEX: usize = 2;

/// The rename dialog's title.
const RENAME_TITLE: &str = "Rename session";
/// The create-space dialog's title — the add-space row's press.
const CREATE_SPACE_TITLE: &str = "New space";
/// The sheet's three chip-level rename dialogs' titles.
const RENAME_SPACE_TITLE: &str = "Rename space";
const RENAME_TAB_TITLE: &str = "Rename tab";
const RENAME_PANE_TITLE: &str = "Rename pane";
/// …and what a landed detach says on the way back to the list.
const DETACHED_TOAST: &str = "detached";
/// What a control a read-only credential may not use says when it is reached
/// anyway — the overflow menu has no disabled row state, so the row refuses.
const READ_ONLY_TOAST: &str = "read-only session";

/// The query key `/terminal?session=<id>` carries the attached session's id
/// under. Transcribed rather than imported (a feature never imports a sibling's
/// `presentation`); `tests/terminal.rs` asserts the round trip.
const SESSION_QUERY_KEY: &str = "session";

/// The wrap padding's wide-window target — `super::chrome::WRAP_PAD`'s 14px
/// scaled up past [`WIDE_BREAKPOINT_PX`]. Bespoke geometry, not a
/// `crate::ui::spacing` tier.
///
/// **The card stays full-bleed at any width** — no content-column clamp, only
/// more breathing room. Applied as an *extra* [`Padding`] outside
/// `terminal_stage` rather than by widening `WRAP_PAD`, which also drives the
/// fullscreen transition's animated-to-zero geometry.
const WIDE_WRAP_PAD_PX: f64 = 22.0;

/// The portrait window's bar-to-card breathing room: a **top-only** gap, applied
/// at the same outside-the-stage site as [`WIDE_WRAP_PAD_PX`]. A wide window is
/// already spaced by its all-round step-up; a narrow one sat flush. The sides
/// keep [`WRAP_PAD`], and fullscreen takes neither (the stage's own progress
/// animation owns that geometry).
const PORTRAIT_TOP_GAP_PX: f64 = WIDE_WRAP_PAD_PX - WRAP_PAD;

const _: () = {
    assert!(WIDE_WRAP_PAD_PX > WRAP_PAD);
    // …so neither orientation can end up flush under the bar again.
    assert!(PORTRAIT_TOP_GAP_PX > 0.0);
};

/// The Terminal screen's route entry point: a `Component` under the outer
/// [`AppState`], so its `init` runs under a real reactive `Owner`.
pub fn terminal_page(overlays: NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(TerminalScreen { overlays }))
}

/// The Terminal screen `Component`. Stateless configuration — everything
/// mutable lives in [`TerminalState`] and the controller it hosts.
pub struct TerminalScreen {
    /// The root overlay host's navigator, handed in by the route builder.
    overlays: NavigatorController<AppState>,
}

/// The Terminal screen's retained state: the action bundle, and the live pane
/// once one has been entered — the state-split rule in practice, since `live`'s
/// `Rc`-backed seams cannot live on the controller at all (see [`LiveSession`]).
pub struct TerminalState {
    /// Everything this screen can *do*, and the handles it needs to do it.
    pub actions: Actions,
    /// The mounted live session's seams, feed and pump — `None` until the first
    /// `build` that observes `TerminalController::live`, `Some` and never `None`
    /// again. Visible to `super::widgets` for one reader: the link strip's
    /// **Retry**, whose callback takes `&mut TerminalState`.
    pub(super) live: Option<LiveSession>,
}

/// Every navigation and mutation this screen performs, plus the handles they
/// need — the bundle that crosses the component-state boundary.
///
/// A `Component`'s subtree is bound to its own `State`, so this screen's gestures
/// take `&mut TerminalState`. Its **overlays** are pages on the app's root
/// overlay host (which is what keeps the `⋮` menu from clipping under the
/// floating bar), so their callbacks receive `&mut AppState` and cannot see
/// [`TerminalState`] at all — hence one cloneable bundle either side calls the
/// same method through. The app bar's `⋮` is a step further out: its handler
/// sees only `&mut AppState`, so this feature publishes its `Actions` in a
/// `thread_local!` while mounted ([`open_terminal_menu`] is the one reader) —
/// the `crate::routes::app_nav` idiom, for the same reason: `provide_context`
/// requires `Send + Sync`, which an `Rc`-backed `NavigatorController` is not.
#[derive(Clone)]
pub struct Actions {
    /// Which mount of the screen this bundle belongs to — see [`TERMINAL_CHROME`].
    id: u64,
    /// The root overlay host's navigator — every page this screen pushes lands
    /// above the app bar rather than under it.
    pub overlays: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how this screen pops back to
    /// `/sessions`, and how it publishes the bar's fullscreen flag.
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<TerminalController>,
    /// The app's one connection handle — read for the live client the feed
    /// attaches with, and for the read-only posture gating this screen's controls.
    pub connection: Arc<ConnectionController>,
    /// Where the app bar's trailing `⋮` slot is painting, in window coordinates —
    /// written by `super::chrome::terminal_stage` on every layout.
    pub anchor: Rc<Cell<Rect>>,
    /// Whether a detach has already claimed this screen's one pop — shared across
    /// every clone, so one mount leaves `/terminal` at most once.
    detaching: Rc<Cell<bool>>,
    /// A test/diagnostic mirror of the live pane's emulator + feed, if one has
    /// been entered. A shared cell because `Actions` is cloned into
    /// [`TERMINAL_CHROME`] at `init`, before any `LiveSession` exists.
    live_probe: Rc<RefCell<Option<LiveProbe>>>,
    /// The live grid's content-tap/wheel hooks, once a live session exists — see
    /// [`GridHooks`]. Same shared-cell shape as [`live_probe`](Self::live_probe).
    grid_hooks: Rc<RefCell<Option<GridHooks>>>,
}

/// A test/diagnostic handle onto the live pane's emulator + feed, reached through
/// the mounted screen's real composition root ([`debug_live_probe`]) so a
/// headless test asserts against the actual mounted session.
#[derive(Clone)]
pub struct LiveProbe {
    /// The VT emulator the live grid is rendering.
    pub emulator: term::SharedEmulator,
    /// The demo feed the grid's pump is draining — concrete, so
    /// [`DemoFeed::last_resize`]/[`DemoFeed::fed_bytes`] are reachable. `None` on
    /// a real `muxrd` session.
    pub feed: Option<Rc<RefCell<DemoFeed>>>,
}

/// The mounted screen's live pane, if one has been entered — `None` before
/// that, or when no terminal screen is mounted at all.
pub fn debug_live_probe() -> Option<LiveProbe> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|actions| actions.live_probe.borrow().clone())
    })
}

/// A test/diagnostic handle onto the live grid's gesture hooks, so a headless
/// test invokes the **exact** closure `TerminalScreen::build` wires the grid to.
/// [`resolve_tap_action`]/[`resolve_wheel_route`] document what each routes to.
#[derive(Clone, Default)]
pub struct GridHooks {
    /// `None` on herdr, whose single-pane stream has nothing a tap could focus.
    pub on_content_tap: Option<term::ContentTapSink>,
    /// `Some` for every live session, both backends.
    pub on_wheel: Option<term::WheelSink>,
}

/// The mounted screen's live grid hooks, once a live session has been entered
/// — `None` before that, or with no terminal screen mounted.
pub fn debug_grid_hooks() -> Option<GridHooks> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|actions| actions.grid_hooks.borrow().clone())
    })
}

thread_local! {
    /// Test-only override for the live pane's Copy sink; `None` leaves
    /// `term::copy_chip`'s system-clipboard sink in place.
    static DEMO_COPY_SINK: RefCell<Option<term::CopySink>> = const { RefCell::new(None) };
}

/// Override the live demo pane's clipboard sink for a headless test, so
/// `Copy` is assertable without the real OS clipboard; `None` restores it.
pub fn set_demo_copy_sink(sink: Option<term::CopySink>) {
    DEMO_COPY_SINK.with(|cell| *cell.borrow_mut() = sink);
}

/// The current override, if a test installed one — see [`set_demo_copy_sink`].
fn demo_copy_sink() -> Option<term::CopySink> {
    DEMO_COPY_SINK.with(|cell| cell.borrow().clone())
}

thread_local! {
    /// The mounted terminal screen's [`Actions`], or `None` when no terminal
    /// screen is on the stack (see [`Actions`] for why a thread-local). Tagged
    /// with the mount's own [`Actions::id`] so a cleanup withdraws only **its
    /// own** entry: frust builds an incoming page before tearing the outgoing one
    /// down, and an unguarded cleanup would blank the live screen's `⋮`.
    static TERMINAL_CHROME: RefCell<Option<Actions>> = const { RefCell::new(None) };
}

/// Mints [`Actions::id`]s. Process-wide and monotonic.
static NEXT_CHROME_ID: AtomicU64 = AtomicU64::new(1);

/// Test/diagnostic accessor: the mounted screen's controller, if any — read
/// through the same [`TERMINAL_CHROME`] handle [`open_terminal_menu`] uses, so a
/// headless test talks to the *mounted* screen's view model. A `#[cfg(test)]`
/// hook would be invisible to those integration binaries.
pub fn debug_controller() -> Option<Arc<TerminalController>> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|actions| Arc::clone(&actions.controller))
    })
}

/// Open the terminal's overflow menu — what `crate::shell` forwards the app bar's
/// `⋮` press to. A press with no terminal screen mounted is silently ignored (the
/// bar carries this action on `/terminal` alone).
pub fn open_terminal_menu(_state: &mut AppState) {
    let actions = TERMINAL_CHROME.with(|cell| cell.borrow().clone());
    if let Some(actions) = actions {
        actions.open_menu();
    }
}

impl Component for TerminalScreen {
    type State = TerminalState;

    fn init(&self) -> TerminalState {
        // Every app-scoped handle, resolved here and stored — never from a press handler.
        let app_nav = app_nav();
        // The attached session's id, read **here** because `init` is the one
        // moment `Nav::param` is exact: this push is what recorded the query.
        let session = app_nav.param(SESSION_QUERY_KEY);
        // The screen-scoped view model over the three app-scoped repositories
        // `di` published (`sessions`'/`settings`' through their own `di.rs` — the
        // sanctioned cross-feature widening). Auto-disposed on unmount.
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

        // The one place the mock-vs-live decision is taken: a screen carrying a
        // session id *and* a live client behind it mounts the grid from the
        // start; everything else keeps the mock transcript.
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
            anchor: Rc::new(Cell::new(Rect::ZERO)),
            detaching: Rc::new(Cell::new(false)),
            live_probe: Rc::new(RefCell::new(None)),
            grid_hooks: Rc::new(RefCell::new(None)),
        };
        // Publish for the app bar's `⋮` (see [`Actions`]' own docs).
        TERMINAL_CHROME.with(|cell| *cell.borrow_mut() = Some(actions.clone()));

        // `on_cleanup`'s closure is `Send + Sync`-bounded, which the `Rc`-backed
        // `Nav` can never satisfy — hence the flag's own small handle.
        let bar_fullscreen = actions.app_nav.fullscreen();
        let id = actions.id;
        on_cleanup(move || {
            // Leaving fullscreen set would strand a hidden bar on the route below.
            bar_fullscreen.set(false);
            // …and, for the identical reason, a hidden OS status/nav bar: every
            // exit that is not `toggle_fullscreen`'s collapse branch is an
            // unmount, and this is the choke point they share. Unconditional:
            // re-requesting a restored mode costs a no-op poll, not a flicker.
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

    /// The screen: `crate::shell`'s floating bar over a `.term-wrap` card (status
    /// row, optional link strip, staggered body) — `docs/terminal.html` v2 is the
    /// pixel source.
    ///
    /// **Pane-content switch.** The card's body (and the key bar's mount) branch
    /// on [`TerminalController::live`]: a real attached session mounts the grid
    /// over a `GrpcTerminalFeed` from the first frame (`init` calls
    /// [`TerminalController::attach_live`] when this screen carries a
    /// `?session=<id>` *and* the app holds a live client), while a bare
    /// `/terminal` and every headless suite keep [`card_body`]'s seeded mock
    /// transcript. The third entry is the checked-in fixture replay, which has no
    /// menu row: `tests/terminal_live.rs` enters it through [`debug_controller`].
    ///
    /// [`LiveSession`] is constructed **once**, lazily, here rather than from a
    /// press handler: `build` runs under this screen's reactive `Owner`, which is
    /// what binds `spawn_feed_pump`'s and `use_interval`'s `on_cleanup` to *this*
    /// screen (their Ambient-Owner contract). A press handler runs under no owner
    /// at all on desktop, where such a registration silently leaks.
    fn build(&self, state: &mut TerminalState) -> AnyView<TerminalState> {
        // Owned, not borrowed: the live-pane entry below needs `&mut state`.
        let controller = Arc::clone(&state.actions.controller);

        // Pane-content switch: a tracked read of `live`, so whichever entry
        // flipped it wakes this rebuild. Constructed exactly once per mount.
        if controller.live.get() && state.live.is_none() {
            let session = LiveSession::enter(&controller, &state.actions.connection);
            *state.actions.live_probe.borrow_mut() = Some(LiveProbe {
                emulator: Rc::clone(&session.emulator),
                feed: session.feed.demo().map(Rc::clone),
            });

            // The grid's gesture hooks (`resolve_tap_action`/`resolve_wheel_route`
            // document the routing), built once beside this session's other seams
            // and re-attached per rebuild by a cheap `Rc` clone. On herdr the tap
            // hook stays `None`: a single-pane stream has nothing a tap could
            // focus beyond the claim the grid's own `Down` already makes.
            let on_content_tap: Option<term::ContentTapSink> = if controller.is_herdr_session() {
                None
            } else {
                let controller = Arc::clone(&controller);
                // The tap-miss backstop's own latch — see the closure below.
                let missed = Rc::new(Cell::new(false));
                let sink: term::ContentTapSink = Rc::new(move |col: u32, row: u32| {
                    let resolved = controller
                        .active_tab_entry()
                        .and_then(|tab| resolve_tap_action(&tab, col, row));
                    let Some((pane_id, is_plugin)) = resolved else {
                        // A miss is usually honest (already-focused pane, or a
                        // tap between rects), but an empty/stale layout snapshot
                        // misses too — and then every tap is dead. So the
                        // **first** miss of a mount re-reads the layout once.
                        if !missed.replace(true) {
                            let controller = Arc::clone(&controller);
                            spawn_local(async move { controller.reload_tabs().await });
                        }
                        // Nothing to focus, so this tap is about the pane the
                        // user is already on: raise the keyboard.
                        return true;
                    };
                    let controller = Arc::clone(&controller);
                    spawn_local(async move {
                        controller.focus_pane_from_tap(pane_id, is_plugin).await;
                    });
                    // A tap that *moved* focus is a pointing gesture, not a typing
                    // one — no keyboard. Its optimistic focus move turns the next
                    // tap on the same pane into the `None` arm above, which raises.
                    false
                });
                Some(sink)
            };
            let on_wheel: term::WheelSink = {
                let controller = Arc::clone(&controller);
                let connection = Arc::clone(&state.actions.connection);
                let feed = session.feed_handle();
                Rc::new(move |up: bool, col: u32, row: u32| {
                    let kind = if up { WheelKind::Up } else { WheelKind::Down };
                    let has_mouse_input = connection
                        .server_info
                        .get_untracked()
                        .is_some_and(|info| info.supports(CAP_MOUSE_INPUT));
                    let active_tab = controller.active_tab_entry();
                    match resolve_wheel_route(has_mouse_input, active_tab.as_ref()) {
                        WheelRoute::Mouse => feed.borrow().send_mouse(kind, col, row),
                        WheelRoute::Scroll { pane_id, is_plugin } => {
                            let controller = Arc::clone(&controller);
                            let direction = match kind {
                                WheelKind::Up => ScrollStep::Up,
                                WheelKind::Down => ScrollStep::Down,
                            };
                            spawn_local(async move {
                                controller
                                    .scroll_pane_from_wheel(pane_id, is_plugin, direction)
                                    .await;
                            });
                        }
                        WheelRoute::Drop => {}
                    }
                })
            };
            *state.actions.grid_hooks.borrow_mut() = Some(GridHooks {
                on_content_tap,
                on_wheel: Some(on_wheel),
            });

            state.live = Some(session);
        }
        // Resize propagation: drain whatever the *previous* layout recorded (a
        // rotation, a font-size change, the fullscreen transition) and forward it
        // to the feed — the app-side stand-in for `SIGWINCH`. A same-size layout
        // leaves `take_pending_resize` at `None`, so this only fires on a delta.
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
        // Gates every control a read-only credential is refused, and puts the grid
        // in its own read-only mode, where no byte leaves the session.
        let read_only = state.actions.connection.is_read_only.get();

        // Two vocabularies, one theme read: the app's scheme-derived palette for
        // the chrome, the card's own `GlyphInk` tones for the card.
        let theme = active_theme();
        let colors = Palette::from_theme(&theme);
        let ink = CardInk::from_theme(&theme);

        // Responsive width-clamp pass: read metrics once, branch on one page
        // constant, never touch layout again. `None` takes the narrow branch.
        // Both non-fullscreen branches pad *outside* the stage, and fullscreen's
        // zero is spelled as insets rather than as *no* wrapper: a `Padding` that
        // comes and goes reconciles as a different child and would tear the stage
        // — the fullscreen transition's driver included — down mid-flight.
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

        // The card's body: the mock transcript, or the live grid once entered.
        let body: AnyView<TerminalState> = match state.live.as_ref() {
            Some(session) => {
                // Tracked: the pump's one write per wake is what repaints the
                // live pane (`term`'s ~30 Hz coalescing contract).
                let generation = session.generation.get();
                let mut grid = term::terminal_grid(
                    Rc::clone(&session.emulator),
                    generation,
                    term::terminal_palette(&theme),
                    controller.font_size.get(),
                )
                .cursor(controller.cursor_style.get())
                .font_family(controller.font_family.get())
                // The grid's accessible name is the pane it shows — the same title
                // the status row prints; empty until the first load lands.
                .label(title.clone())
                .input(session.input.clone())
                .read_only(read_only)
                .selection(session.selection.clone());
                if let Some(hooks) = state.actions.grid_hooks.borrow().clone() {
                    if let Some(sink) = hooks.on_content_tap {
                        grid = grid.on_content_tap(sink);
                    }
                    if let Some(sink) = hooks.on_wheel {
                        grid = grid.on_wheel(sink);
                    }
                }
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
            None => card_body(lines, epoch, &ink),
        };

        // The link strip sits between the status row and the body, and only when
        // there is something to say: a healthy (or absent) link adds no child.
        let mut card_children = vec![inflexible(status_row(
            &title,
            fullscreen,
            &colors,
            &ink,
            toggle_fullscreen,
        ))];
        if let Some(strip) = link_strip(link, &colors, &ink) {
            card_children.push(inflexible(strip));
        }
        card_children.push(flexible(1, body));

        let card =
            FlexView::new(Axis::Vertical, card_children).cross_axis(CrossAxisAlignment::Stretch);

        let stage = super::chrome::terminal_stage(
            card,
            fullscreen,
            ink.card,
            ink.card_border,
            Rc::clone(&state.actions.anchor),
        );
        // Wrapped from the outside rather than by widening `terminal_stage`'s own
        // animated padding, which fullscreen must still animate to exactly zero.
        let stage: AnyView<TerminalState> = any(Padding(extra_wrap_pad, stage));

        // The page tone stays **outside** the stage so it fills the whole surface,
        // bar band included: the bar slides in over it, and a page that stopped at
        // the clearance would flash the shell's clear colour through the gap.
        let page: AnyView<TerminalState> = any(term_box(stage, colors.void, 0.0));

        // Key bar mount: a fixed slot beneath the card, live-mode only. The `None`
        // arm returns `page` unwrapped rather than adding an always-zero sibling.
        match state.live.as_ref() {
            Some(session) => {
                // The keyboard's reported height is the app's one proxy for "the
                // IME is up" — this screen has no other text input, so it mirrors
                // `keybar`'s own internal read rather than inventing a second one.
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

/// Hit-test a content tap's cell against `tab`'s pane rectangles — the first half
/// of the grid's `on_content_tap` routing (see [`resolve_tap_action`]).
///
/// **Fullscreen wins outright**: if any pane is
/// [`is_fullscreen`](PaneEntry::is_fullscreen), every tap belongs to it.
/// **Floating beats tiled** past that check — the original's active-layer
/// preference. An approximation, recorded rather than modeled: the wire carries
/// no per-pane z-order beyond [`PaneEntry::floating`], so "topmost" is read as
/// the last pane in wire order whose rect contains the cell, within its layer.
fn resolve_tapped_pane(tab: &TabEntry, col: u32, row: u32) -> Option<&PaneEntry> {
    if let Some(fullscreen) = tab.panes.iter().find(|pane| pane.is_fullscreen) {
        return Some(fullscreen);
    }
    let contains = |pane: &&PaneEntry| {
        col >= pane.x && col < pane.x + pane.cols && row >= pane.y && row < pane.y + pane.rows
    };
    tab.panes
        .iter()
        .filter(|pane| pane.floating)
        .rfind(contains)
        .or_else(|| {
            tab.panes
                .iter()
                .filter(|pane| !pane.floating)
                .rfind(contains)
        })
}

/// The `(pane_id, is_plugin)` a content tap should focus, if any — the live
/// grid's whole `on_content_tap` routing decision.
///
/// **Tap → FocusPane.** A tap hit-tests the server-active tab's pane rectangles
/// ([`resolve_tapped_pane`]) and resolves to a focus call only when it lands on a
/// *different*, non-fullscreen pane; the already-focused pane and a no-hit tap
/// both answer `None`. A landed focus reloads the tab list
/// ([`TerminalController::focus_pane_from_tap`]) so the rects and the sheet's
/// badge converge on what the server did.
///
/// **Tap → keyboard** is the same answer read the other way: the hook returns
/// *should this tap raise the keyboard?* ([`term::ContentTapSink`]), so `Some`
/// focuses and answers `false` and `None` answers `true` — tapping an unfocused
/// pane twice focuses, then types, with no extra state. herdr, whose hook is
/// never wired, keeps the grid's unconditional tap → keyboard; a drag-scroll
/// never reaches this hook. The focus *claim* is separate, made by the grid's own
/// `Down` whatever this answers.
fn resolve_tap_action(tab: &TabEntry, col: u32, row: u32) -> Option<(u32, bool)> {
    let pane = resolve_tapped_pane(tab, col, row)?;
    if pane.focused {
        return None;
    }
    Some((pane.id, pane.is_plugin))
}

/// Where one wheel step should go — see [`resolve_wheel_route`].
#[derive(Debug)]
enum WheelRoute {
    /// Send a mouse frame at the wheel's own cell — the primary route, gated on
    /// the server capability alone; which pane is under the pointer is its call.
    Mouse,
    /// RPC-fallback: scroll the SERVER-ACTIVE tab's *focused* pane, not whatever
    /// pane the pointer is over (original-app parity).
    Scroll {
        /// The focused pane's [`PaneEntry::id`].
        pane_id: u32,
        /// The focused pane's [`PaneEntry::is_plugin`].
        is_plugin: bool,
    },
    /// No capability and no focused pane to fall back to — dropped silently
    /// rather than guessed at.
    Drop,
}

/// Decide [`WheelRoute`] for one wheel step, given whether the server advertised
/// [`CAP_MOUSE_INPUT`] and the server-active tab (`None` before the first load).
///
/// A server with the capability gets a `mouse` frame straight off the attach
/// stream at the wheel's own cell, so it routes the step to whichever pane sits
/// underneath (alt-screen apps like `htop` included). Without it, the step falls
/// back to [`TerminalController::scroll_pane_from_wheel`] against the active
/// tab's *focused* pane — never the pane under the pointer — and a step with no
/// focused pane is dropped.
fn resolve_wheel_route(has_mouse_input: bool, active_tab: Option<&TabEntry>) -> WheelRoute {
    if has_mouse_input {
        return WheelRoute::Mouse;
    }
    match active_tab.and_then(|tab| tab.panes.iter().find(|pane| pane.focused)) {
        Some(pane) => WheelRoute::Scroll {
            pane_id: pane.id,
            is_plugin: pane.is_plugin,
        },
        None => WheelRoute::Drop,
    }
}

/// The expand button's press, as the screen-state shim its callback needs.
fn toggle_fullscreen(state: &mut TerminalState) {
    state.actions.toggle_fullscreen();
}

/// Give the OS status/navigation bars back ([`SystemUiMode::EdgeToEdge`], the
/// platform default) — the one restore helper every fullscreen exit calls:
/// [`Actions::toggle_fullscreen`]'s collapse branch, and `init`'s [`on_cleanup`],
/// which every *other* exit funnels through as an unmount.
///
/// **Edge-triggered, never build-time**: `frust::set_system_ui_mode` bumps a
/// process-global generation counter a shell watcher polls each frame, so driving
/// it from `build` would re-request the same mode every frame.
fn restore_system_ui() {
    set_system_ui_mode(SystemUiMode::EdgeToEdge);
}

impl Actions {
    /// Flip fullscreen: one flag, three build-time consumers —
    ///
    /// 1. the **card** animates its top clearance (`safe-area + BAR_HEIGHT` →
    ///    `safe-area`), wrap padding (14 → 0) and corner radius (22 → 0) in one
    ///    gesture (`super::chrome::terminal_stage`);
    /// 2. the **button** swaps its vector mark and takes the amber on-state;
    /// 3. the **shell's bar** animates away via [`Nav::set_fullscreen`] — the
    ///    bar's existing hidden/reveal driver, not a second hide path.
    ///
    /// The OS status/nav bars are the fourth follower and the one that is *not* a
    /// build-time read — see [`restore_system_ui`]. **Unmount restores the bar**:
    /// the flag outlives this screen, so `init`'s [`on_cleanup`] clears it rather
    /// than stranding a hidden bar on `/sessions`.
    pub fn toggle_fullscreen(&self) {
        let on = self.controller.toggle_fullscreen();
        self.app_nav.set_fullscreen(on);
        if on {
            // Immersive-sticky, not plain immersive: an edge swipe reveals the
            // bars transiently and they auto-hide again.
            set_system_ui_mode(SystemUiMode::ImmersiveSticky);
        } else {
            restore_system_ui();
        }
    }

    /// Open the app bar's overflow menu, anchored to the bar slot the `⋮` actually
    /// paints in.
    ///
    /// `show_glyph_menu` takes a **caller-reported** window-coordinate rect (there
    /// is no ancestor-bounds query a widget can make), and the slot belongs to
    /// `crate::ui::topbar`, which this page cannot reach into — so
    /// `super::chrome::terminal_stage`, which knows both the window box and the
    /// status-bar inset, computes the rect during layout and drops it in
    /// [`anchor`](Self::anchor) for this to read back.
    ///
    /// The menu is a transparent push onto the **root overlay host**, so it paints
    /// over the bar rather than clipping under it and the bar goes inert for free.
    /// It paints no scrim of its own by design: it is not a dimming modal.
    ///
    /// **The menu claims root focus on every press, and this app cannot stop it**
    /// — a framework-side gap: the widget requests focus on any pointer `Down` so
    /// a subsequent Escape has a chain to travel, with no opt-out. The claim
    /// summons no keyboard and leaves with the menu.
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
    /// The body builder is re-invoked on every rebuild while the sheet is up, so
    /// its reads are tracked and a browse shows with no hand-rolled rebuild.
    /// Opening points the browse back at the tab the session is on, and
    /// **refreshes the layout** — the only way this sheet ever shows a tab or pane
    /// opened elsewhere (there is no server push). The open is *not* blocked on
    /// that round trip: the fresh answer patches in through [`sheet_body`]'s
    /// tracked reads, so a slow link costs a late pill, never a stuck sheet.
    pub fn open_sheet(&self) {
        self.controller.begin_browsing();
        let actions = self.clone();
        show_sheet(&self.overlays, move |sheet| sheet_body(&actions, sheet));
        let controller = Arc::clone(&self.controller);
        spawn_local(async move { controller.reload_tabs().await });
    }

    /// Point the sheet at the tab keyed `key`: local, synchronous and RPC-free.
    /// Nothing about the session moves and the sheet stays up — only
    /// [`commit_pane`](Self::commit_pane) does either.
    pub fn browse_tab(&self, key: &str) {
        self.controller.browse_tab(key.to_string());
    }

    /// Commit the sheet's browse: focus the pane `pane_id` of the tab keyed
    /// `tab_key`, then close the sheet a beat later so the selection visibly moves
    /// first (`setTimeout(closeSheet, 260)`).
    ///
    /// The sheet's **one** navigating gesture and its one dismissal, in two
    /// phases. While a commit is **in flight**, a second row is dropped at the
    /// controller's single-flight claim — **first tap wins**, and the dropped tap
    /// schedules nothing. Once the commit has landed and the close beat is
    /// pending, the sheet stays live and a repeat tap commits again —
    /// [`SheetHandle::close_after`] re-times the one pending close rather than
    /// stacking a second behind it (**latest tap wins**, only inside the beat),
    /// and the superseded wait cannot pop the `/terminal` route.
    ///
    /// Two commits are refused client-side for a **read-only** credential, each
    /// because the server would otherwise answer with silence: one that has to move
    /// **tabs** first (the wire's `GoToTab` is mutating, so the sequence would stop
    /// there anyway), and any commit on **herdr**, whose relay drops an inbound
    /// `FocusPane` from a read-only client without a word. Zellij's same-tab commit
    /// is `FocusPane` alone, which that backend honours as the read it is.
    ///
    /// The stream is never touched here: the verbs [`TerminalController::commit_pane`]
    /// sends are what move the session, on either backend.
    pub fn commit_pane(&self, sheet: &SheetHandle, tab_key: &str, pane_id: u32, is_plugin: bool) {
        // Untracked: a press handler has no reactive owner on desktop.
        let cross_tab = tab_key != self.controller.active_tab.get_untracked();
        let herdr = self.controller.is_herdr_session();
        if (cross_tab || herdr) && self.connection.is_read_only.get_untracked() {
            app_toasts().show(READ_ONLY_TOAST);
            return;
        }
        let controller = Arc::clone(&self.controller);
        let sheet = sheet.clone();
        let tab_key = tab_key.to_string();
        spawn_local(async move {
            if !controller.commit_pane(tab_key, pane_id, is_plugin).await {
                // False covers two cases, both leaving the sheet up with no close
                // scheduled: a refused verb (its failure toast is already on its
                // way) and a tap dropped by the single-flight claim (silent — the
                // first tap's commit still owns the close).
                return;
            }
            sheet.close_after(SHEET_CLOSE_DELAY).await;
        });
    }

    /// Switch this connection's view to the space with id `id`, reloading the tab
    /// world behind it. The sheet stays up.
    ///
    /// `SwitchSpace` is **MUTATING** on the wire, so a read-only credential is
    /// refused here rather than at the server — the same pre-emption the rename
    /// dialog and a herdr commit make, and for the same reason: the pill stays on
    /// screen and says why. Every *other* refusal is still the server's, and
    /// reaches the toast bridge through the controller's own failure sink.
    pub fn select_space(&self, id: &str) {
        // Untracked: a press handler has no reactive owner on desktop.
        if self.connection.is_read_only.get_untracked() {
            app_toasts().show(READ_ONLY_TOAST);
            return;
        }
        let controller = Arc::clone(&self.controller);
        let id = id.to_string();
        spawn_local(async move { controller.select_space(id).await });
    }

    /// Open the rename dialog, seeded with the attached session's *current* name —
    /// re-read from the controller rather than captured, so a menu opened several
    /// frames ago cannot seed a stale name. With no attached session the dialog
    /// does not open. A rename is **mutating**, so a read-only credential is
    /// refused here rather than at the server: the row stays and says why.
    pub fn open_rename_dialog(&self) {
        // Untracked throughout: a press handler has no reactive owner on desktop.
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
    /// **One pop per mount**, latched through [`Actions::detaching`]: the detach is
    /// async and the menu can be reopened while it is in flight, so a second
    /// landing pop would take `/sessions` off the stack too. A refusal re-arms it.
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

    /// Open the create-space dialog — the add-space row's press (herdr only;
    /// [`sheet_body`] withdraws the row on a backend with no spaces, and for a
    /// read-only credential). Its non-blank guard is the only client-side check.
    pub fn open_create_space_dialog(&self) {
        let controller = Arc::clone(&self.controller);
        name_input_dialog(
            &self.overlays,
            CREATE_SPACE_TITLE,
            "",
            move |_app: &mut AppState, label: String| {
                let controller = Arc::clone(&controller);
                spawn_local(async move {
                    controller.create_space(label).await;
                });
            },
        );
    }

    /// Open the tab-naming dialog — the add-tab row's press. Confirm creates: a
    /// typed name reaches the server as typed, and a blank field is a legitimate
    /// choice (the server's own default naming); cancel creates nothing. The body
    /// is [`new_tab_dialog`], hand-rolled for the reason its own doc records.
    pub fn open_new_tab_dialog(&self) {
        let controller = Arc::clone(&self.controller);
        let value = RwSignal::new(String::new());
        let page_nav = self.overlays.clone();
        show_glyph_dialog(
            &self.overlays,
            move || new_tab_dialog(value, &page_nav),
            move |_app: &mut AppState, result: PopResult| {
                if let Some(name) = result.take::<String>() {
                    let controller = Arc::clone(&controller);
                    spawn_local(async move {
                        controller.new_tab(name).await;
                    });
                }
            },
        );
    }

    /// Open the rename dialog for the space `id`, prefilled with its current
    /// `label` — the space pill's trailing edit icon (herdr only, withdrawn for a
    /// read-only credential). Reloads only the space list: a label touches no tab.
    pub fn open_rename_space_dialog(&self, id: &str, label: &str) {
        let controller = Arc::clone(&self.controller);
        let id = id.to_string();
        name_input_dialog(
            &self.overlays,
            RENAME_SPACE_TITLE,
            label,
            move |_app: &mut AppState, label: String| {
                let controller = Arc::clone(&controller);
                let id = id.clone();
                spawn_local(async move {
                    controller.rename_space(id, label).await;
                });
            },
        );
    }

    /// Open the rename dialog for the tab `tab_id`, prefilled with its current
    /// `name` — the tab pill's trailing edit icon. Reloads only the tab list: a
    /// full load would replay the reveal for nothing.
    pub fn open_rename_tab_dialog(&self, tab_id: u64, name: &str) {
        let controller = Arc::clone(&self.controller);
        name_input_dialog(
            &self.overlays,
            RENAME_TAB_TITLE,
            name,
            move |_app: &mut AppState, name: String| {
                let controller = Arc::clone(&controller);
                spawn_local(async move {
                    controller.rename_tab(tab_id, name).await;
                });
            },
        );
    }

    /// Open the rename dialog for the pane `pane_id`, prefilled with its current
    /// `title` — the pane row's trailing edit icon. Reloads only the tab list,
    /// since a pane's title lives inside [`TabEntry::panes`].
    pub fn open_rename_pane_dialog(&self, pane_id: u32, is_plugin: bool, title: &str) {
        let controller = Arc::clone(&self.controller);
        name_input_dialog(
            &self.overlays,
            RENAME_PANE_TITLE,
            title,
            move |_app: &mut AppState, name: String| {
                let controller = Arc::clone(&controller);
                spawn_local(async move {
                    controller.rename_pane(pane_id, is_plugin, name).await;
                });
            },
        );
    }

    /// Open a new pane in the tab the session is on — tiled on a tap, floating on
    /// a long-press. A backend that ignores the flag still gets it.
    pub fn new_pane(&self, floating: bool) {
        let controller = Arc::clone(&self.controller);
        spawn_local(async move {
            controller.new_pane(floating).await;
        });
    }
}

/// The overflow menu's entries. A free fn because `show_glyph_menu` takes a
/// `Fn() -> Vec<MenuEntry>` it may re-invoke per rebuild.
///
/// **No icons**: `MenuEntry` is a label plus a normal/danger variant with no icon
/// slot, and a glyph as label text is the font-fallback trap `crate::ui::topbar`'s
/// vector rule forbids. *Detach* is deliberately not the danger variant —
/// detaching leaves the session running. **No separators either**: three
/// ungrouped rows have nothing to punctuate, and a separator's 11px band read on
/// device as uneven row spacing.
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
        // An off-by-one into the entry list runs the wrong action silently.
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
        // The mock's own 260ms: long enough to see the pill move.
        assert_eq!(SHEET_CLOSE_DELAY.as_millis(), 260);
    }

    // `resolve_tapped_pane` — the live grid's content-tap hit test.

    /// A pane at `(x, y)`-`(x+w, y+h)` cells, `id` unique within its namespace.
    fn pane_at(id: u32, x: u32, y: u32, w: u32, h: u32) -> PaneEntry {
        PaneEntry {
            id,
            title: String::new(),
            cwd: String::new(),
            focused: false,
            floating: false,
            exited: false,
            is_plugin: false,
            is_fullscreen: false,
            x,
            y,
            rows: h,
            cols: w,
        }
    }

    fn tab_with(panes: Vec<PaneEntry>) -> TabEntry {
        TabEntry {
            index: 1,
            tab_id: 1,
            key: "devbox".to_string(),
            label: "devbox".to_string(),
            pane_title: String::new(),
            pane_name: String::new(),
            pane_path: String::new(),
            panes,
            active: true,
        }
    }

    #[test]
    fn a_tap_resolves_to_the_tiled_pane_whose_rect_contains_the_cell() {
        let tab = tab_with(vec![pane_at(10, 0, 0, 40, 24), pane_at(11, 40, 0, 40, 24)]);
        assert_eq!(resolve_tapped_pane(&tab, 5, 5).map(|p| p.id), Some(10));
        assert_eq!(resolve_tapped_pane(&tab, 45, 5).map(|p| p.id), Some(11));
    }

    #[test]
    fn a_tap_outside_every_rect_resolves_to_nothing() {
        let tab = tab_with(vec![pane_at(10, 0, 0, 40, 24)]);
        assert_eq!(resolve_tapped_pane(&tab, 40, 0), None, "one past the edge");
        assert_eq!(
            resolve_tapped_pane(&tab, 0, 24),
            None,
            "one past the bottom"
        );
        assert!(tab_with(Vec::new()).panes.is_empty());
        assert_eq!(resolve_tapped_pane(&tab_with(Vec::new()), 5, 5), None);
    }

    #[test]
    fn a_fullscreen_pane_claims_every_tap_regardless_of_where_it_landed() {
        let mut fullscreen = pane_at(20, 0, 0, 10, 10);
        fullscreen.is_fullscreen = true;
        let tab = tab_with(vec![pane_at(10, 40, 40, 10, 10), fullscreen]);
        // Neither cell is inside the fullscreen pane's own (tiny) rect.
        assert_eq!(resolve_tapped_pane(&tab, 0, 0).map(|p| p.id), Some(20));
        assert_eq!(resolve_tapped_pane(&tab, 45, 45).map(|p| p.id), Some(20));
    }

    #[test]
    fn a_floating_pane_wins_over_a_tiled_one_it_overlaps() {
        let tiled = pane_at(10, 0, 0, 40, 24);
        let mut floating = pane_at(11, 0, 0, 40, 24);
        floating.floating = true;
        let tab = tab_with(vec![tiled, floating]);
        assert_eq!(
            resolve_tapped_pane(&tab, 5, 5).map(|p| p.id),
            Some(11),
            "floating beats tiled on the same cell"
        );
    }

    #[test]
    fn the_floating_preference_only_applies_where_a_floating_pane_actually_hits() {
        let tiled = pane_at(10, 0, 0, 40, 24);
        let mut floating = pane_at(11, 60, 60, 10, 10);
        floating.floating = true;
        let tab = tab_with(vec![tiled, floating]);
        assert_eq!(
            resolve_tapped_pane(&tab, 5, 5).map(|p| p.id),
            Some(10),
            "the floating pane exists but does not cover this cell, so tiled \
             still answers"
        );
    }

    // `resolve_tap_action` — adds the already-focused early-out.

    #[test]
    fn a_tap_on_another_pane_answers_its_id_and_namespace() {
        let other = pane_at(11, 40, 0, 40, 24);
        let mut focused = pane_at(10, 0, 0, 40, 24);
        focused.focused = true;
        let tab = tab_with(vec![focused, other]);
        assert_eq!(resolve_tap_action(&tab, 45, 5), Some((11, false)));
    }

    #[test]
    fn a_tap_on_the_already_focused_pane_answers_nothing() {
        let mut focused = pane_at(10, 0, 0, 40, 24);
        focused.focused = true;
        let tab = tab_with(vec![focused, pane_at(11, 40, 0, 40, 24)]);
        assert_eq!(
            resolve_tap_action(&tab, 5, 5),
            None,
            "the Down that reached this tap already claimed focus — nothing \
             extra to do"
        );
    }

    #[test]
    fn a_stale_or_no_hit_tap_answers_nothing() {
        let tab = tab_with(vec![pane_at(10, 0, 0, 40, 24)]);
        assert_eq!(resolve_tap_action(&tab, 90, 90), None);
        assert_eq!(resolve_tap_action(&tab_with(Vec::new()), 0, 0), None);
    }

    // `resolve_wheel_route` — the live grid's wheel hook's routing.

    #[test]
    fn capability_present_always_routes_to_a_mouse_frame() {
        let mut focused = pane_at(10, 0, 0, 40, 24);
        focused.focused = true;
        let tab = tab_with(vec![focused]);
        assert!(matches!(
            resolve_wheel_route(true, Some(&tab)),
            WheelRoute::Mouse
        ));
        assert!(matches!(resolve_wheel_route(true, None), WheelRoute::Mouse));
    }

    #[test]
    fn capability_absent_falls_back_to_the_active_tabs_focused_pane() {
        let mut focused = pane_at(10, 0, 0, 40, 24);
        focused.focused = true;
        let other = pane_at(11, 40, 0, 40, 24);
        let tab = tab_with(vec![other, focused]);
        match resolve_wheel_route(false, Some(&tab)) {
            WheelRoute::Scroll { pane_id, is_plugin } => {
                assert_eq!(pane_id, 10);
                assert!(!is_plugin);
            }
            other => panic!("expected a Scroll route, got a different one: {other:?}"),
        }
    }

    #[test]
    fn capability_absent_with_nothing_focused_drops_the_step() {
        let tab = tab_with(vec![pane_at(10, 0, 0, 40, 24)]);
        assert!(matches!(
            resolve_wheel_route(false, Some(&tab)),
            WheelRoute::Drop
        ));
        assert!(matches!(resolve_wheel_route(false, None), WheelRoute::Drop));
    }
}
