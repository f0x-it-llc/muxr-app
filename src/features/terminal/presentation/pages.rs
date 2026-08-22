//! The `/terminal` route's page: muxr's terminal surface, a pixel copy of
//! `docs/terminal.html` v2 — no tabs strip, path bar, cmd hint, or FAB. The
//! app bar's menu button opens the "panes & tabs" side sheet directly
//! (`crate::shell` forwards its press to [`open_terminal_menu`]); the pane
//! title sits in the card's status row.
//!
//! [`terminal_page`] is what `crate::routes` mounts — a per-screen
//! [`Component`] whose `init` hosts a [`TerminalController`] via
//! `use_controller`, whose `build` is pure arrangement over
//! [`super::widgets`]. Contracts live where enforced: fullscreen on
//! [`Actions::toggle_fullscreen`]/[`restore_system_ui`], the state boundary
//! on [`Actions`], the mock-vs-live switch on [`TerminalScreen::build`],
//! gesture routing on [`resolve_tap_action`]/[`resolve_wheel_route`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::authoring::WindowInsets;
use frust::authoring::text::FontStyle;
use frust::{
    AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, GestureDetector, Get,
    GetUntracked, IconData, NavigatorController, Padding, PopResult, RwSignal, SystemUiMode,
    WindowMetrics, any, component, container, flexible, icon, icons, inflexible, on_cleanup,
    scroll_view, set_system_ui_mode, spawn_local, text, use_context,
};
use frust_glyph::show_glyph_dialog;

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
use crate::ui::sheet::SheetHandle;
use crate::ui::side_sheet::show_side_sheet;
use crate::ui::spacing;
use crate::ui::toasts::app_toasts;

use super::chrome::WRAP_PAD;
use super::controller::{LiveSession, TerminalController};
use super::keybar;
use super::term;
use super::widgets::card::{CardInk, card_body};
use super::widgets::link_strip::link_strip;
use super::widgets::new_tab_dialog::new_tab_dialog;
use super::widgets::side_sheet_tree::{
    TabPath, TreeActions, TreeData, TreeMutations, auto_expand_after_cascade, tree_body,
};
use super::widgets::status::status_row;
use super::widgets::util::{column, h_gap, row, sized, v_gap};

/// How long the sheet stays up after a **pane row** is tapped, so the selection
/// visibly moves before it closes — `setTimeout(closeSheet, 260)` in the mock. A
/// pane tap is the sheet's one dismissing gesture ([`Actions::commit_pane`]).
const SHEET_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(260);

/// The side sheet's header label.
const SHEET_TITLE: &str = "panes & tabs";
/// The sticky footer's two session actions — the exact rows the overflow menu
/// used to carry.
const FOOTER_RENAME_LABEL: &str = "Rename session";
const FOOTER_DETACH_LABEL: &str = "Detach session";
/// The footer's hold-to-edit discoverability tip — the mandated affordance
/// now that no row carries its own trailing edit icon.
const FOOTER_TIP: &str = "hold a row to rename";

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
/// anyway — the sheet has no disabled row state, so the row refuses.
const READ_ONLY_TOAST: &str = "read-only session";

/// The query key `/terminal?session=<id>` carries the attached session's id
/// under. Transcribed rather than imported (a feature never imports a sibling's
/// `presentation`); `tests/terminal.rs` asserts the round trip.
const SESSION_QUERY_KEY: &str = "session";

/// The wrap padding's wide-window target — `WRAP_PAD`'s 14px scaled up past
/// [`WIDE_BREAKPOINT_PX`]. Bespoke geometry, not a `crate::ui::spacing` tier.
/// The card stays full-bleed at any width (more breathing room, no
/// content-column clamp), carried as a *gutter* the stage adds to `WRAP_PAD`
/// and animates to zero with it rather than by widening `WRAP_PAD` itself.
const WIDE_WRAP_PAD_PX: f64 = 22.0;

/// The portrait window's **top-only** bar-to-card gap, the same gutter input
/// as [`WIDE_WRAP_PAD_PX`] — a narrow window sat flush otherwise. Sides keep
/// [`WRAP_PAD`]; fullscreen takes neither.
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
/// need — the bundle crossing the component-state boundary.
///
/// A `Component`'s subtree is bound to its `State`, so gestures take
/// `&mut TerminalState`. Overlay pages (the root host, so the side sheet
/// doesn't clip under the floating bar) get `&mut AppState` instead and
/// can't see [`TerminalState`] — hence one cloneable bundle both sides call
/// through. The app bar's menu button is a step further out still: its
/// handler sees only `&mut AppState`, so this publishes `Actions` in a
/// `thread_local!` while mounted ([`open_terminal_menu`] reads it) — the
/// `app_nav` idiom, since `provide_context` needs `Send + Sync`, which `Rc`
/// isn't.
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

/// Open the terminal's "panes & tabs" side sheet — what `crate::shell` forwards
/// the app bar's menu-button press to, directly (no intervening dropdown). A
/// press with no terminal screen mounted is silently ignored (the bar carries
/// this action on `/terminal` alone).
pub fn open_terminal_menu(_state: &mut AppState) {
    let actions = TERMINAL_CHROME.with(|cell| cell.borrow().clone());
    if let Some(actions) = actions {
        actions.open_sheet();
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
            // …and the OS status/nav bar, for the same reason: every
            // non-collapse exit is an unmount sharing this choke point;
            // unconditional costs only a no-op poll, never a flicker.
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

    /// The screen: `crate::shell`'s floating bar over a `.term-wrap` card
    /// (status row, optional link strip, staggered body) —
    /// `docs/terminal.html` v2 is the pixel source.
    ///
    /// **Pane-content switch** on [`TerminalController::live`]: a real
    /// attached session mounts the grid from the first frame (`init` calls
    /// [`TerminalController::attach_live`] when carrying `?session=<id>`
    /// with a live client); a bare `/terminal` keeps [`card_body`]'s mock.
    /// The fixture replay (no menu row) is entered via [`debug_controller`],
    /// as `tests/terminal_live.rs` does.
    ///
    /// [`LiveSession`] is built **once**, lazily, here rather than in a
    /// press handler: `build` runs under this screen's `Owner`, which is
    /// what binds `spawn_feed_pump`'s/`use_interval`'s `on_cleanup` — a
    /// press handler has no owner at all on desktop.
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

            // Gesture hooks (routing documented on `resolve_tap_action`/
            // `resolve_wheel_route`), built once, re-attached per rebuild by
            // an `Rc` clone. `None` on herdr: nothing a tap could focus
            // beyond the grid's own `Down` claim.
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

        // Responsive width-clamp: read metrics once, branch once. These are the
        // card's windowed gutters only — the stage animates them to zero itself
        // (`super::chrome::terminal_stage`), which is why they are handed to it
        // rather than wrapped around it.
        let metrics = use_context::<WindowMetrics>();
        let is_wide = metrics
            .map(|m| m.size.width >= WIDE_BREAKPOINT_PX)
            .unwrap_or(false);
        let gutters = if is_wide {
            EdgeInsets::all(WIDE_WRAP_PAD_PX - WRAP_PAD)
        } else {
            EdgeInsets {
                left: 0.0,
                top: PORTRAIT_TOP_GAP_PX,
                right: 0.0,
                bottom: 0.0,
            }
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

        let stage: AnyView<TerminalState> = any(super::chrome::terminal_stage(
            card,
            fullscreen,
            gutters,
            ink.card,
            ink.card_border,
        ));

        // The page tone stays **outside** the stage so it fills the whole surface,
        // bar band included: the bar slides in over it, and a page that stopped at
        // the clearance would flash the shell's clear colour through the gap.
        let page: AnyView<TerminalState> = any(container(stage).fill(colors.void));

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

/// Hit-test a content tap's cell against `tab`'s pane rectangles — the
/// first half of [`resolve_tap_action`]'s routing.
///
/// **Fullscreen wins outright** ([`is_fullscreen`](PaneEntry::is_fullscreen));
/// **floating beats tiled** past that. An approximation: the wire carries no
/// per-pane z-order beyond [`PaneEntry::floating`], so "topmost" is the last
/// pane in wire order whose rect contains the cell, within its layer.
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
/// **Tap → FocusPane**: hit-tests via [`resolve_tapped_pane`], resolving
/// only when the tap lands on a *different*, non-fullscreen pane (the
/// focused pane and a miss both answer `None`). A landed focus reloads the
/// tab list ([`TerminalController::focus_pane_from_tap`]) to converge the
/// rects and the sheet's badge.
///
/// **Tap → keyboard** reads the same answer inverted
/// ([`term::ContentTapSink`]): `Some` focuses without raising the
/// keyboard, `None` raises it — so tapping an unfocused pane twice
/// focuses, then types. Never wired for herdr, which keeps the grid's
/// unconditional tap → keyboard; the focus *claim* itself is separate,
/// made by the grid's own `Down`.
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

/// Decide [`WheelRoute`] for one wheel step: [`CAP_MOUSE_INPUT`] present
/// routes a `mouse` frame at the wheel's cell (whatever pane sits
/// underneath, alt-screen apps included); absent, it falls back to
/// [`TerminalController::scroll_pane_from_wheel`] on the active tab's
/// *focused* pane — never the pointer's — dropped if none is focused.
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

/// Give the OS status/nav bars back ([`SystemUiMode::EdgeToEdge`]) — the
/// one restore helper every fullscreen exit calls: the collapse branch of
/// [`Actions::toggle_fullscreen`], and every other exit via `init`'s
/// [`on_cleanup`]. **Edge-triggered, never build-time**: bumps a
/// process-global generation a shell watcher polls, so a `build`-time call
/// would re-request every frame.
fn restore_system_ui() {
    set_system_ui_mode(SystemUiMode::EdgeToEdge);
}

impl Actions {
    /// Flip fullscreen: one flag, four followers — the **card** animates its
    /// clearance/padding/radius (`super::chrome::terminal_stage`), the
    /// **button** swaps its mark, the **shell's bar** hides via
    /// [`Nav::set_fullscreen`] (its existing driver), and the OS status/nav
    /// bars follow via [`restore_system_ui`] — the one *not* build-time read.
    ///
    /// **Unmount restores the bar**: the flag outlives this screen, so
    /// `init`'s [`on_cleanup`] clears it rather than stranding it on `/sessions`.
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

    /// Open the "panes & tabs" side sheet — what the app bar's menu button
    /// now opens directly, with no intervening dropdown.
    ///
    /// The tree's own body/footer builders re-invoke on every rebuild while
    /// the sheet is up, so a browse shows tracked with no hand-rolled
    /// rebuild. Opening seeds the tree's resting expansion and re-points the
    /// browse at the session's tab, then **refreshes the layout** (no server
    /// push otherwise) without blocking the open: the answer patches in
    /// through the tree's tracked reads, so a slow link costs a late row,
    /// never a stuck sheet. Dismissal — whichever gesture drove it — resets
    /// the whole browse via [`TerminalController::reset_browse`], so nothing
    /// survives to the next open.
    pub fn open_sheet(&self) {
        self.controller.begin_browsing();
        self.controller.seed_expansion();
        let body_actions = self.clone();
        let footer_actions = self.clone();
        let reset_controller = Arc::clone(&self.controller);
        show_side_sheet(
            &self.overlays,
            SHEET_TITLE,
            move |sheet| sheet_tree_body(&body_actions, sheet),
            move |sheet| sheet_footer(&footer_actions, sheet),
            move || reset_controller.reset_browse(),
        );
        let controller = Arc::clone(&self.controller);
        spawn_local(async move { controller.reload_tabs().await });
    }

    /// Toggle the tree sheet's space disclosure: collapse if `expanded` (the
    /// row's own current state), else expand it — and, on a landed expand,
    /// auto-drill the space's active tab open once its own cascade settles.
    /// [`auto_expand_after_cascade`] gives the policy (target tab + delay);
    /// this is the wiring that owns the controller and spawns the timer.
    pub fn toggle_tree_space(&self, id: &str, expanded: bool) {
        if expanded {
            self.controller.collapse_space();
            return;
        }
        let controller = Arc::clone(&self.controller);
        let id = id.to_string();
        let can_create = !self.connection.is_read_only.get_untracked();
        spawn_local(async move {
            controller.expand_space(id.clone()).await;
            // The stale guard: a user who has since expanded a different
            // space (or collapsed this one) leaves nothing to auto-drill.
            if controller.expanded_space.get_untracked().as_deref() != Some(id.as_str()) {
                return;
            }
            let tabs = match controller.browsed_space_tabs.get_untracked() {
                Some((cached_id, cached_tabs)) if cached_id == id => cached_tabs,
                _ => controller.tabs.get_untracked(),
            };
            let Some((target, delay)) = auto_expand_after_cascade(&tabs, can_create) else {
                return;
            };
            clean_signals::time::sleep(delay).await;
            if controller.expanded_space.get_untracked().as_deref() == Some(id.as_str()) {
                controller.expand_tab(target);
            }
        });
    }

    /// Toggle the tree sheet's tab disclosure — synchronous and RPC-free like
    /// [`TerminalController::expand_tab`]/[`collapse_tab`](TerminalController::collapse_tab):
    /// every pane rode in with its own tab.
    pub fn toggle_tree_tab(&self, key: &str, expanded: bool) {
        if expanded {
            self.controller.collapse_tab();
        } else {
            self.controller.expand_tab(key.to_string());
        }
    }

    /// Commit the tree sheet's browse: a row under the space this connection
    /// is already on takes [`commit_pane`](Self::commit_pane) unchanged
    /// (`path.space_id` is `None` on zellij, which has no spaces at all); a
    /// row under any **other** space takes
    /// [`TerminalController::commit_pane_in_space`] instead, which is
    /// **always** herdr and so always mutating — refused up front for a
    /// read-only credential, the same posture [`commit_pane`](Self::commit_pane)
    /// takes for every herdr commit regardless of which tab it targets.
    pub(crate) fn commit_tree_pane(
        &self,
        sheet: &SheetHandle,
        path: &TabPath,
        pane_id: u32,
        is_plugin: bool,
    ) {
        let Some(space_id) = &path.space_id else {
            self.commit_pane(sheet, &path.tab_key, pane_id, is_plugin);
            return;
        };
        if self.connection.is_read_only.get_untracked() {
            app_toasts().show(READ_ONLY_TOAST);
            return;
        }
        let controller = Arc::clone(&self.controller);
        let sheet = sheet.clone();
        let space_id = space_id.clone();
        let tab_key = path.tab_key.clone();
        spawn_local(async move {
            if !controller
                .commit_pane_in_space(space_id, tab_key, pane_id, is_plugin)
                .await
            {
                return;
            }
            sheet.close_after(SHEET_CLOSE_DELAY).await;
        });
    }

    /// Commit the sheet's browse: focus pane `pane_id` of tab `tab_key`,
    /// then close the sheet a beat later (`setTimeout(closeSheet, 260)`) so
    /// the selection visibly moves first. The sheet's one navigating gesture
    /// and its one dismissal.
    ///
    /// **First tap wins**: a commit in flight drops a second row at the
    /// controller's single-flight claim. **Latest tap wins** inside the
    /// close beat: [`SheetHandle::close_after`] re-times the one pending
    /// close rather than stacking a second.
    ///
    /// Refused client-side for a **read-only** credential when it needs a
    /// tab move first (`GoToTab` is mutating) or targets **herdr** (whose
    /// relay silently drops a read-only `FocusPane`); Zellij's same-tab
    /// commit is a plain read, so it's exempt.
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

    /// Open the rename dialog, seeded with the attached session's *current* name —
    /// re-read from the controller rather than captured, so a sheet opened several
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
    /// async and the sheet can be reopened while it is in flight, so a second
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

    /// Open the create-space dialog — the tree's add-space row press (herdr
    /// only; [`tree_body`] withdraws the row on a backend with no spaces, and
    /// for a read-only credential). Its non-blank guard is the only
    /// client-side check.
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
    /// `label` — the tree's space row press-and-hold (herdr only, withdrawn for a
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
    /// `name` — the tree's tab row press-and-hold. Reloads only the tab list: a
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
    /// `title` — the tree's pane row press-and-hold. Reloads only the tab list,
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

    /// Open a new pane in the tab the session is on, `floating` or tiled. A
    /// backend that ignores the flag still gets it. The tree's add-pane row
    /// wires only the tiled path (`floating = false`) — it has no long-press
    /// affordance, unlike the flat sheet's own row it replaced.
    pub fn new_pane(&self, floating: bool) {
        let controller = Arc::clone(&self.controller);
        spawn_local(async move {
            controller.new_pane(floating).await;
        });
    }
}

/// Build the tree sheet's read side from the controller's own tracked
/// signals — the [`TreeData`] borrow [`tree_body`] renders from — and hand it
/// straight to `tree_body` alongside [`tree_actions`]'s wiring. A free fn
/// (rather than an `Actions` method) since it needs to borrow four owned
/// locals across the call, which a method returning `AnyView` can't do.
fn sheet_tree_body(actions: &Actions, sheet: &SheetHandle) -> AnyView<AppState> {
    let controller = &actions.controller;
    let spaces = controller.spaces.get();
    let tabs = controller.tabs.get();
    let browsed = controller.browsed_space_tabs.get();
    let expanded_space = controller.expanded_space.get();
    let expanded_tab = controller.expanded_tab.get();
    let has_spaces = controller.has_spaces();
    let data = TreeData {
        spaces: &spaces,
        tabs: &tabs,
        browsed: browsed.as_ref(),
        expanded_space: expanded_space.as_deref(),
        expanded_tab: expanded_tab.as_deref(),
        has_spaces,
    };
    // The side sheet hands its body slot a tight extent and clips whatever
    // overflows it — the tree itself scrolls no more than `tree_body` on its
    // own does, so a session with many spaces/tabs/panes stays fully
    // reachable rather than running off the bottom of the panel.
    any(scroll_view(tree_body(&data, &tree_actions(actions, sheet))))
}

/// Wire the tree's [`TreeActions`]/[`TreeMutations`] onto this screen's
/// existing dialog-opener/commit/new-pane handlers — every gesture the tree
/// can fire, bridged from its view-only callbacks to `&mut AppState`
/// (this sheet is a page of the root overlay host, like every other overlay
/// this screen pushes).
fn tree_actions(actions: &Actions, sheet: &SheetHandle) -> TreeActions<AppState> {
    let read_only = actions.connection.is_read_only.get();

    let toggle_space_actions = actions.clone();
    let toggle_tab_actions = actions.clone();
    let commit_actions = actions.clone();
    let commit_sheet = sheet.clone();

    let mutations = (!read_only).then(|| {
        let rename_space_actions = actions.clone();
        let rename_tab_actions = actions.clone();
        let rename_pane_actions = actions.clone();
        let new_space_actions = actions.clone();
        let new_tab_actions = actions.clone();
        let new_pane_actions = actions.clone();
        TreeMutations {
            on_rename_space: Rc::new(move |_app: &mut AppState, id: &str, label: &str| {
                rename_space_actions.open_rename_space_dialog(id, label);
            }),
            on_rename_tab: Rc::new(move |_app: &mut AppState, tab_id: u64, label: &str| {
                rename_tab_actions.open_rename_tab_dialog(tab_id, label);
            }),
            on_rename_pane: Rc::new(
                move |_app: &mut AppState, pane_id: u32, is_plugin: bool, title: &str| {
                    rename_pane_actions.open_rename_pane_dialog(pane_id, is_plugin, title);
                },
            ),
            on_new_space: Rc::new(move |_app: &mut AppState| {
                new_space_actions.open_create_space_dialog();
            }),
            // The controller creates a tab in this connection's own selected
            // space; the tree's per-space add-tab row carries no way to
            // target a *different* one yet (the flat sheet it replaced had
            // the same one-space limit).
            on_new_tab: Rc::new(move |_app: &mut AppState, _space: Option<&str>| {
                new_tab_actions.open_new_tab_dialog();
            }),
            // Likewise: the controller always creates in the tab the session
            // is on, so a create row under a merely-browsed tab still lands
            // there rather than at `path`.
            on_new_pane: Rc::new(move |_app: &mut AppState, _path: &TabPath| {
                new_pane_actions.new_pane(false);
            }),
        }
    });

    TreeActions {
        on_space: Rc::new(move |_app: &mut AppState, id: &str, expanded: bool| {
            toggle_space_actions.toggle_tree_space(id, expanded);
        }),
        on_tab: Rc::new(move |_app: &mut AppState, key: &str, expanded: bool| {
            toggle_tab_actions.toggle_tree_tab(key, expanded);
        }),
        on_pane: Rc::new(
            move |_app: &mut AppState, path: &TabPath, pane_id: u32, is_plugin: bool| {
                commit_actions.commit_tree_pane(&commit_sheet, path, pane_id, is_plugin);
            },
        ),
        mutations,
    }
}

/// The side sheet's sticky footer: the two session actions the overflow menu
/// used to carry (rename/detach) plus the hold-to-edit discoverability tip —
/// see [`FOOTER_TIP`].
fn sheet_footer(actions: &Actions, sheet: &SheetHandle) -> AnyView<AppState> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);

    let rename = {
        let actions = actions.clone();
        let sheet = sheet.clone();
        footer_action(
            icons::EDIT,
            FOOTER_RENAME_LABEL,
            &colors,
            move |_app: &mut AppState| {
                // Same reason `sessions::manage_sheet`'s own Rename/Kill rows
                // chain through `close_then` rather than a bare `close`: this
                // pushes a dialog onto the sheet's own overlay host, and an
                // unchained push would land ahead of the sheet's staged exit.
                let actions = actions.clone();
                sheet.close_then(move || actions.open_rename_dialog());
            },
        )
    };
    let detach = {
        let actions = actions.clone();
        let sheet = sheet.clone();
        footer_action(
            icons::LOGOUT,
            FOOTER_DETACH_LABEL,
            &colors,
            move |_app: &mut AppState| {
                // Detach pops the **route** stack, a different navigator than
                // this sheet's overlay host — a plain `close` (not `close_then`)
                // carries no ordering hazard, the same shape Attach takes there.
                sheet.close();
                actions.detach();
            },
        )
    };
    let mut tip_style = sized(&theme.type_scale.body_small, FOOTER_TIP_SIZE, colors.fg_dim);
    tip_style.style = FontStyle::Italic;

    any(column(vec![
        rename,
        v_gap(spacing::XS),
        detach,
        v_gap(spacing::SM),
        any(text(FOOTER_TIP).style(tip_style)),
    ]))
}

/// The footer's icon size, in logical px — smaller than the tree's own row
/// marks, since this is a two-line action stack, not a disclosure level.
const FOOTER_ICON_SIZE: f64 = 16.0;
/// The footer's action label / tip text sizes.
const FOOTER_ACTION_SIZE: f32 = 12.0;
const FOOTER_TIP_SIZE: f32 = 10.0;

/// One footer action: a leading vector mark (never a font glyph —
/// `crate::ui::topbar`'s chrome-icon rule) plus a label, the whole row
/// tappable.
fn footer_action(
    mark: impl Into<IconData>,
    label: &'static str,
    colors: &Palette,
    on_press: impl Fn(&mut AppState) + 'static,
) -> AnyView<AppState> {
    let theme = active_theme();
    let content = row(vec![
        inflexible(icon(mark).size(FOOTER_ICON_SIZE).color(colors.fg_dim)),
        h_gap(spacing::SM),
        inflexible(text(label).style(sized(
            &theme.type_scale.label_large,
            FOOTER_ACTION_SIZE,
            colors.fg,
        ))),
    ]);
    any(GestureDetector(Padding(EdgeInsets::symmetric(0.0, spacing::XS), content)).on_tap(on_press))
}

#[cfg(test)]
mod tests {
    use super::*;

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
