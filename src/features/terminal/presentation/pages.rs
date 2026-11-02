//! The `/terminal` route's page: muxr's terminal surface, a pixel copy of
//! `docs/terminal.html` v2 — no tabs strip, path bar, cmd hint, or FAB. The
//! app bar's menu button opens the "panes & tabs" side sheet directly
//! (`crate::shell` forwards its press to [`open_terminal_menu`]); the pane
//! title sits in the card's status row. On a Tablet the tree is docked
//! instead, as [`super::widgets::rail`] beside the card ([`super::actions`]
//! holds that rail's glue).
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
use frust::authoring::text::FontStyle;
use frust::authoring::{Rect, Size, WindowInsets};
use frust::{
    Axis, Component, CrossAxisAlignment, EdgeInsets, Either, FlexView, Get, GetUntracked, IconData,
    NavigatorController, PopResult, RwSignal, Set, SystemUiMode, View, WindowMetrics, column,
    component, container, flexible, icons, inflexible, on_cleanup, row, set_system_ui_mode,
    spawn_local, text, use_context,
};
use frust_glyph::{show_glyph_dialog, show_glyph_menu};
use frust_haptics::{HapticEffect, Haptics};

use crate::AppState;
use crate::connection::{CAP_MOUSE_INPUT, ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::settings::di::expect_preferences_repository;
use crate::features::terminal::data::feeds::demo_feed::DemoFeed;
use crate::features::terminal::di::expect_terminal_repository;
use crate::features::terminal::domain::entities::{AgentEntry, PaneEntry, ScrollStep, TabEntry};
use crate::features::terminal::domain::feed::WheelKind;
use crate::responsive::Responsive;
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::name_input_dialog;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::SheetHandle;
use crate::ui::side_sheet::show_side_sheet;
use crate::ui::spacing;
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::{BAR_HEIGHT, icon_button};

use super::actions::{AfterCommit, rail_view};
use super::chrome::WRAP_PAD;
use super::controller::{LiveSession, TerminalController};
use super::keybar;
use super::term;
use super::widgets::agents_section::{
    AgentsData, FocusedPane, PressAgent, agents_section, tree_split,
};
use super::widgets::card::{CardInk, card_body};
use super::widgets::link_strip::link_strip;
use super::widgets::new_tab_dialog::new_tab_dialog;
use super::widgets::rail::MiniActions;
use super::widgets::side_sheet_tree::{
    PressMenu, RowTarget, TabPath, TreeActions, TreeData, TreeMode, TreeMutations,
    auto_expand_after_cascade, tree_body,
};
use super::widgets::status::{
    PanesControl, StatusOptions, TerminalMenuChoice, status_row, terminal_menu,
    terminal_menu_choice,
};
use super::widgets::util::{centered_row, h_gap, sized, stretch_column, v_gap};

/// How long the sheet stays up after a **pane row** is tapped, so the selection
/// visibly moves before it closes — `setTimeout(closeSheet, 260)` in the mock. A
/// pane tap is the sheet's one dismissing gesture ([`Actions::commit_pane`]).
pub(super) const SHEET_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(260);

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

/// The wrap padding's wide-window target — `WRAP_PAD`'s 14px scaled up past
/// [`crate::responsive::WIDE_BREAKPOINT_PX`]. Bespoke geometry, not a `crate::ui::spacing` tier.
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
pub fn terminal_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    component(TerminalScreen { overlays })
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
    /// Whether the last `build` laid out the Tablet structure (the docked rail).
    tablet: Rc<Cell<bool>>,
    /// The bar's trailing slot in window coordinates, as the last `build` derived
    /// it — the Tablet menu's anchor (the shell forwards the press with none).
    menu_anchor: Rc<Cell<Rect>>,
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
    /// `None` on herdr and on a read-only credential; see [`resolve_pinch_target`].
    pub on_pinch: Option<term::PinchSink>,
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
    // The Tablet's docked rail is the tree already, so its kebab is a menu.
    if let Some(actions) = actions {
        if actions.tablet.get() {
            actions.open_terminal_menu();
        } else {
            actions.open_sheet();
        }
    }
}

impl Component for TerminalScreen {
    type State = TerminalState;

    fn init(&self) -> TerminalState {
        // Every app-scoped handle, resolved here and stored — never from a press handler.
        let app_nav = app_nav();
        // The attached session's id, read **here** because `init` is the one
        // moment `Nav::param` is exact: this push is what recorded the query.
        let session = app_nav.param(crate::routes::TERMINAL_SESSION_QUERY_KEY);
        let tab = app_nav
            .param(crate::routes::TERMINAL_TAB_QUERY_KEY)
            .and_then(|value| value.parse::<u32>().ok());
        let pane = app_nav
            .param(crate::routes::TERMINAL_PANE_QUERY_KEY)
            .and_then(|value| value.parse::<u32>().ok());
        let space = app_nav
            .param(crate::routes::TERMINAL_SPACE_QUERY_KEY)
            .filter(|value| !value.is_empty());
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

        controller.set_attach_target_in_space(space, tab, pane);

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

        let tablet = Responsive::of(use_context::<WindowMetrics>()).is_tablet();
        controller.set_docked(tablet);

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
            tablet: Rc::new(Cell::new(tablet)),
            menu_anchor: Rc::new(Cell::new(Rect::ZERO)),
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
    fn build(&self, state: &mut TerminalState) -> impl View<TerminalState> {
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
            // A credential's read-only status is fixed for the mount, so an
            // untracked read here is the whole gate.
            let on_pinch: Option<term::PinchSink> = if controller.is_herdr_session()
                || state.actions.connection.is_read_only.get_untracked()
            {
                None
            } else {
                let controller = Arc::clone(&controller);
                let selection = session.selection.clone();
                Some(Rc::new(
                    move |intent: term::PinchIntent, focal: frust::authoring::Point| {
                        let Some(cell) = selection.cell_at(focal) else {
                            return;
                        };
                        let Some(tab) = controller.active_tab_entry() else {
                            return;
                        };
                        let Some((pane_id, is_plugin)) =
                            resolve_pinch_target(&tab, intent, cell.col.into(), cell.row.into())
                        else {
                            return;
                        };
                        let _ = Haptics::perform(HapticEffect::SelectionClick);
                        let controller = Arc::clone(&controller);
                        spawn_local(async move {
                            controller
                                .toggle_pane_fullscreen_from_pinch(pane_id, is_plugin)
                                .await;
                        });
                    },
                ))
            };
            *state.actions.grid_hooks.borrow_mut() = Some(GridHooks {
                on_content_tap,
                on_wheel: Some(on_wheel),
                on_pinch,
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

        // Responsive: read metrics once, branch on the one rule. The size class
        // docks the tree; the clamp tier is the card's windowed gutters only —
        // the stage animates them to zero itself (`super::chrome::terminal_stage`),
        // which is why they are handed to it rather than wrapped around it.
        let responsive = Responsive::of(use_context::<WindowMetrics>());
        let tablet = responsive.is_tablet();
        state.actions.tablet.set(tablet);
        if let Some(metrics) = use_context::<WindowMetrics>() {
            state.actions.menu_anchor.set(bar_trailing_slot(&metrics));
        }
        if controller.set_docked(tablet) {
            let controller = Arc::clone(&controller);
            spawn_local(async move {
                if tablet {
                    controller.settle_rest();
                } else {
                    controller.reset_browse();
                }
            });
        }
        let gutters = if responsive.wide_clamp {
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
        let body = match state.live.as_ref() {
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
                let mut pinch = None;
                if let Some(hooks) = state.actions.grid_hooks.borrow().clone() {
                    if let Some(sink) = hooks.on_content_tap {
                        grid = grid.on_content_tap(sink);
                    }
                    if let Some(sink) = hooks.on_wheel {
                        grid = grid.on_wheel(sink);
                    }
                    pinch = hooks.on_pinch;
                }
                let mut chip = term::copy_chip(session.selection.clone());
                if let Some(sink) = demo_copy_sink() {
                    chip = chip.copy_sink(sink);
                }
                Either::Left(term::terminal_surface_with_pinch(
                    session.selection.clone(),
                    grid,
                    chip,
                    pinch,
                ))
            }
            None => Either::Right(card_body(lines, epoch, &ink)),
        };

        // The link strip sits between the status row and the body, and only when
        // there is something to say: a healthy (or absent) link adds no child.
        // Crumbs on the Tablet's Full band only; the collapsed band has no room.
        let crumbs = if tablet && responsive.is_full() {
            controller.crumb_labels()
        } else {
            Vec::new()
        };
        // The panes control is a Compact affordance: the Tablet's rail already
        // carries the tree, so its status row keeps the one exit button.
        let panes = (fullscreen && !tablet).then(|| {
            PanesControl::new(|state: &mut TerminalState| state.actions.open_sheet())
                .with_attention(controller.has_blocked_agent())
        });
        let mut card_children = vec![inflexible(status_row(
            &title,
            &crumbs,
            StatusOptions {
                fullscreen,
                read_only,
                panes,
            },
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

        let stage =
            super::chrome::terminal_stage(card, fullscreen, gutters, ink.card, ink.card_border);

        // The page tone stays **outside** the stage so it fills the whole surface,
        // bar band included: the bar slides in over it, and a page that stopped at
        // the clearance would flash the shell's clear colour through the gap.
        let page = container(stage).fill(colors.void);
        let page = if tablet {
            let mini = fullscreen || !responsive.is_full();
            let rail = rail_view(&state.actions, mini, fullscreen, read_only, link);
            Either::Left(
                row()
                    .child(rail)
                    .flex(1, page)
                    .cross_axis(CrossAxisAlignment::Stretch),
            )
        } else {
            Either::Right(page)
        };

        // Key bar mount: a fixed slot beneath the card, live-mode only. The `None`
        // arm returns `page` unwrapped rather than adding an always-zero sibling.
        match state.live.as_ref() {
            Some(session) => {
                let bar = if tablet {
                    // Persistent behind the menu toggle; read-only never shows it.
                    let shown = controller.key_bar_visible.get() && !read_only;
                    let fn_row = controller.fn_row_visible;
                    Either::Left(keybar::tablet_key_bar(
                        shown,
                        responsive.band,
                        fn_row,
                        &session.input,
                    ))
                } else {
                    // The keyboard's reported height is the app's one proxy for "the
                    // IME is up" — this screen has no other text input, so it mirrors
                    // `keybar`'s own internal read rather than inventing a second one.
                    let keyboard_up = use_context::<WindowInsets>()
                        .map(|insets| insets.view_insets.bottom > 0.0)
                        .unwrap_or(false);
                    Either::Right(keybar::key_bar(keyboard_up, &session.input))
                };
                Either::Left(
                    column()
                        .flex(1, page)
                        .child(bar)
                        .cross_axis(CrossAxisAlignment::Stretch),
                )
            }
            None => Either::Right(page),
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
/// **floating beats tiled** past that, but only while the tab's floating layer is
/// visible ([`TabEntry::floating_panes_visible`]) — a hidden floating pane is not
/// hit-testable. An approximation: the wire carries no per-pane z-order beyond
/// [`PaneEntry::floating`], so "topmost" is the last pane in wire order whose rect
/// contains the cell, within its layer.
///
/// Floating panes are hit-testable only while the tab's floating layer is visible — a hidden
/// full-size floating pane must never capture a tap or pinch (zellij shows the layer when a
/// floating pane is focused).
fn resolve_tapped_pane(tab: &TabEntry, col: u32, row: u32) -> Option<&PaneEntry> {
    if let Some(fullscreen) = tab.panes.iter().find(|pane| pane.is_fullscreen) {
        return Some(fullscreen);
    }
    let contains = |pane: &&PaneEntry| {
        col >= pane.x && col < pane.x + pane.cols && row >= pane.y && row < pane.y + pane.rows
    };
    tab.panes
        .iter()
        .filter(|pane| pane.floating && tab.floating_panes_visible)
        .rfind(contains)
        .or_else(|| {
            tab.panes
                .iter()
                .filter(|pane| !pane.floating)
                .rfind(contains)
        })
}

/// The last tiled pane whose rect contains the cell — the pinch's hit test, blind to
/// the floating layer and to fullscreen.
fn tiled_pane_at(tab: &TabEntry, col: u32, row: u32) -> Option<&PaneEntry> {
    tab.panes
        .iter()
        .filter(|pane| !pane.floating)
        .rfind(|pane| {
            col >= pane.x && col < pane.x + pane.cols && row >= pane.y && row < pane.y + pane.rows
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

/// The `(pane_id, is_plugin)` a pinch should toggle fullscreen on, if any.
///
/// **Spread** is a no-op while the tab is fullscreen (its own flag or any pane's);
/// otherwise it targets the *tiled* pane under the focal cell, falling back to the
/// focused tiled pane — never a floating pane. **Converge** is a no-op unless the
/// tab is fullscreen, and targets the focused tiled pane. It never scans
/// `is_fullscreen` first: zellij's `is_focused` is a union over clients, so every
/// client-active pane is flagged while one is fullscreen; muxrd's B-FOCUS override
/// makes `focused` the relay's own pane, the one this connection fullscreened. A
/// flagged tiled pane is only the fallback.
fn resolve_pinch_target(
    tab: &TabEntry,
    intent: term::PinchIntent,
    col: u32,
    row: u32,
) -> Option<(u32, bool)> {
    let fullscreen = tab.fullscreen_active || tab.panes.iter().any(|pane| pane.is_fullscreen);
    let focused_tiled = || tab.panes.iter().find(|pane| pane.focused && !pane.floating);
    let pane = match intent {
        term::PinchIntent::EnterFullscreen => {
            if fullscreen {
                return None;
            }
            tiled_pane_at(tab, col, row).or_else(focused_tiled)?
        }
        term::PinchIntent::ExitFullscreen => {
            if !fullscreen {
                return None;
            }
            focused_tiled().or_else(|| {
                tab.panes
                    .iter()
                    .find(|pane| pane.is_fullscreen && !pane.floating)
            })?
        }
    };
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

/// Glyph's bar icon slot (`.ab-icon` 40px) and its horizontal padding (6px),
/// both private to the bar and transcribed for [`bar_trailing_slot`].
const BAR_ICON_SLOT: f64 = 40.0;
const BAR_PAD_X: f64 = 6.0;

/// The bar's trailing icon slot in window coordinates: the last
/// [`BAR_ICON_SLOT`] of the width inside the trailing safe inset and the bar's
/// own padding, one bar tall below the top inset.
fn bar_trailing_slot(metrics: &WindowMetrics) -> Rect {
    let right = metrics.size.width - metrics.insets.view_padding.right - BAR_PAD_X;
    let top = metrics.insets.view_padding.top;
    Rect::new(right - BAR_ICON_SLOT, top, right, top + BAR_HEIGHT)
}

/// The expand button's press, as the screen-state shim its callback needs.
fn toggle_fullscreen(state: &mut TerminalState) {
    state.actions.toggle_fullscreen();
}

/// The tree's expanded space, read **teardown-safely** — `None` both for "none
/// expanded" and for "this mount is gone", which
/// [`Actions::toggle_tree_space`]'s post-await guards treat the same way.
fn expanded_space(controller: &TerminalController) -> Option<String> {
    controller.expanded_space.try_get_untracked().flatten()
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
    /// rebuild. Opening seeds the tree's resting expansion
    /// ([`TerminalController::seed_expansion`]: the selected space and the
    /// active tab both open pre-drilled), then **refreshes the layout** (no
    /// server push otherwise) without blocking the open: the answer patches in
    /// through the tree's tracked reads, so a slow link costs a late row,
    /// never a stuck sheet. Dismissal — whichever gesture drove it — resets
    /// the whole browse via [`TerminalController::reset_browse`], so nothing
    /// survives to the next open.
    pub fn open_sheet(&self) {
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
            self.controller.close_space(id);
            return;
        }
        let controller = Arc::clone(&self.controller);
        let id = id.to_string();
        let can_create = !self.connection.is_read_only.get_untracked();
        spawn_local(async move {
            controller.expand_space(id.clone()).await;
            // Every read here is `try_*`: this body spans two awaits (the peek,
            // then the cascade's own settle), and the screen can unmount across
            // either — a plain read of a disposed signal panics. A gone owner
            // has nothing left to drill open, so each one returns quietly.
            if expanded_space(&controller).as_deref() != Some(id.as_str()) {
                // The stale guard, and the disposal guard in the same read: a
                // user who has since expanded a different space (or collapsed
                // this one) leaves nothing to auto-drill either.
                return;
            }
            let Some(browsed) = controller.browsed_space_tabs.try_get_untracked() else {
                return;
            };
            let tabs = match browsed {
                Some((cached_id, cached_tabs)) if cached_id == id => cached_tabs,
                _ => match controller.tabs.try_get_untracked() {
                    Some(tabs) => tabs,
                    None => return,
                },
            };
            let Some((target, delay)) = auto_expand_after_cascade(&tabs, can_create) else {
                return;
            };
            clean_signals::time::sleep(delay).await;
            if expanded_space(&controller).as_deref() == Some(id.as_str()) {
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
    /// [`TerminalController::commit_pane_in_space`] instead, which is always
    /// herdr. `SwitchSpace` carries no read-only gate on the wire
    /// (`muxrd/src/grpc/space_ops.rs`'s `switch_space_impl` has no
    /// `reject_if_read_only`), so a read-only credential reaches this the
    /// same as any other caller; a server-side refusal still surfaces as a
    /// toast through [`ack_result`](crate::net::error::ack_result).
    fn commit_tree_pane(&self, after: &AfterCommit, path: &TabPath, pane_id: u32, is_plugin: bool) {
        let Some(space_id) = &path.space_id else {
            self.commit_pane_then(after, &path.tab_key, pane_id, is_plugin);
            return;
        };
        let controller = Arc::clone(&self.controller);
        let after = after.clone();
        let space_id = space_id.clone();
        let tab_key = path.tab_key.clone();
        spawn_local(async move {
            if !controller
                .commit_pane_in_space(space_id, tab_key, pane_id, is_plugin)
                .await
            {
                return;
            }
            after.land(&controller).await;
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
    /// Reachable for a **read-only** credential regardless of tab or backend:
    /// `GoToTab` carries no read-only gate on the wire
    /// (`muxrd/src/grpc/tab_ops.rs`) and neither does `FocusPane`
    /// (`muxrd/src/grpc/pane_ops.rs`), on zellij or herdr alike. A server-side
    /// refusal still surfaces as a toast through
    /// [`ack_result`](crate::net::error::ack_result).
    pub fn commit_pane(&self, sheet: &SheetHandle, tab_key: &str, pane_id: u32, is_plugin: bool) {
        self.commit_pane_then(
            &AfterCommit::Close(sheet.clone()),
            tab_key,
            pane_id,
            is_plugin,
        );
    }

    /// An agent row's tap: jump to its pane through
    /// [`TerminalController::commit_agent`] — the pane row's own commit — then
    /// `after` (the sheet closes, the rail re-rests on the new tab).
    fn commit_agent(&self, after: &AfterCommit, agent: &AgentEntry) {
        let controller = Arc::clone(&self.controller);
        let after = after.clone();
        let agent = agent.clone();
        spawn_local(async move {
            if controller.commit_agent(&agent).await {
                after.land(&controller).await;
            }
        });
    }

    /// [`commit_pane`](Self::commit_pane) with the follow-up `after` picks.
    fn commit_pane_then(&self, after: &AfterCommit, tab_key: &str, pane_id: u32, is_plugin: bool) {
        let controller = Arc::clone(&self.controller);
        let after = after.clone();
        let tab_key = tab_key.to_string();
        spawn_local(async move {
            if !controller.commit_pane(tab_key, pane_id, is_plugin).await {
                // False covers two cases, both leaving the sheet up with no close
                // scheduled: a refused verb (its failure toast is already on its
                // way) and a tap dropped by the single-flight claim (silent — the
                // first tap's commit still owns the close).
                return;
            }
            after.land(&controller).await;
        });
    }

    /// The Tablet kebab's Glyph menu, anchored below the bar's trailing slot.
    /// Its checked item and immersive mark are read as the menu opens.
    fn open_terminal_menu(&self) {
        let controller = &self.controller;
        let (fullscreen, key_bar) = (
            controller.fullscreen.get_untracked(),
            controller.key_bar_visible.get_untracked(),
        );
        let actions = self.clone();
        show_glyph_menu(
            &self.overlays,
            self.menu_anchor.get(),
            move || terminal_menu(fullscreen, key_bar),
            move |_app: &mut AppState, result: PopResult| match result
                .take::<usize>()
                .and_then(terminal_menu_choice)
            {
                Some(TerminalMenuChoice::Rename) => actions.open_rename_dialog(),
                Some(TerminalMenuChoice::Immersive) => actions.toggle_fullscreen(),
                Some(TerminalMenuChoice::KeyBar) => {
                    let visible = &actions.controller.key_bar_visible;
                    visible.set(!visible.get_untracked());
                }
                Some(TerminalMenuChoice::Detach) => actions.detach(),
                None => {}
            },
        );
    }

    /// The mini rail's presses: a space is adopted (its load re-rests the rail),
    /// a tab or pane commits exactly as a tree pane row does.
    pub(super) fn mini_actions(&self) -> MiniActions<TerminalState> {
        let (space, tab, pane, exit) = (self.clone(), self.clone(), self.clone(), self.clone());
        MiniActions {
            on_space: Rc::new(move |_state: &mut TerminalState, id: &str| {
                let controller = Arc::clone(&space.controller);
                let id = id.to_string();
                spawn_local(async move {
                    controller.adopt_space(id).await;
                });
            }),
            on_tab: Rc::new(move |_state: &mut TerminalState, target: &TabEntry| {
                let focused = target.panes.iter().find(|pane| pane.focused);
                if let Some(pane) = focused.or_else(|| target.panes.first()) {
                    tab.commit_pane_then(&AfterCommit::Rest, &target.key, pane.id, pane.is_plugin);
                }
            }),
            on_pane: Rc::new(
                move |_state: &mut TerminalState, key: &str, target: &PaneEntry| {
                    pane.commit_pane_then(&AfterCommit::Rest, key, target.id, target.is_plugin);
                },
            ),
            on_exit_immersive: Rc::new(move |_state: &mut TerminalState| exit.toggle_fullscreen()),
        }
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

    /// The tree's add-tab row: create in the space that row sits under.
    ///
    /// A row under a merely **browsed** space switches to it first — a real
    /// `switch_space`, resume hint and all, exactly the move
    /// [`TerminalController::commit_pane_in_space`] makes — and only then
    /// opens the dialog, so the tab lands where the user pointed rather than
    /// wherever the connection happened to be. That switch is also how an
    /// **empty** space is reached at all: it draws no pane row to commit
    /// through, so its add-tab row is its one way in. `None` (a backend with
    /// no spaces) creates in the only space there is.
    pub fn open_new_tab_dialog_in(&self, space_id: Option<&str>) {
        let Some(space_id) = space_id.map(str::to_string) else {
            self.open_new_tab_dialog();
            return;
        };
        let controller = Arc::clone(&self.controller);
        let actions = self.clone();
        spawn_local(async move {
            // A refused switch says so through its own failure toast, and one
            // dropped at the controller's single-flight claim says nothing at
            // all (first press wins); opening the dialog behind either would
            // create in the wrong space.
            if controller.adopt_space(space_id).await {
                actions.open_new_tab_dialog();
            }
        });
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

    /// The tree's add-pane row: open a tiled pane under the tab `path` names,
    /// moving the session onto that space and tab first when it is browsing
    /// somewhere else ([`TerminalController::new_pane_in`] owns that order).
    /// Only the tiled path is wired (`floating = false`) — this row has no
    /// long-press affordance, unlike the flat sheet's own row it replaced.
    pub(crate) fn new_tree_pane(&self, path: &TabPath) {
        let controller = Arc::clone(&self.controller);
        let (space_id, tab_key) = (path.space_id.clone(), path.tab_key.clone());
        spawn_local(async move {
            controller.new_pane_in(space_id, tab_key, false).await;
        });
    }
}

/// The side sheet's body: [`tree_view`] in its sheet mode, scrolling, and a
/// pane commit closing the sheet behind it.
fn sheet_tree_body(actions: &Actions, sheet: &SheetHandle) -> impl View<AppState> + use<> {
    // This body is a page of the **root** overlay host, so it can outlive the
    // screen that pushed it (a guard bounce off `/terminal` leaves the sheet
    // up). One untracked liveness check ahead of the tracked reads below, which
    // panic on a disposed owner; nothing follows it that could dispose one.
    if !actions.controller.signals_alive() {
        return Either::Left(stretch_column(Vec::new()));
    }
    // The side sheet hands its body slot a tight extent and clips whatever
    // overflows it — the tree itself scrolls no more than `tree_body` on its
    // own does, so a session with many spaces/tabs/panes stays fully
    // reachable rather than running off the bottom of the panel. That tight
    // extent is also what bounds the two halves of the agents split.
    let after = AfterCommit::Close(sheet.clone());
    let agents = agents_view(actions, after.clone());
    Either::Right(tree_split(
        tree_view(actions, TreeMode::Sheet, after),
        agents,
    ))
}

/// The tree's agents half over the controller's tracked agent list, or `None`
/// while the session runs no agent (the host then mounts the tree alone). A tap
/// commits like a pane row, then `after`.
pub(super) fn agents_view<S: 'static>(
    actions: &Actions,
    after: AfterCommit,
) -> Option<impl View<S> + use<S>> {
    let controller = &actions.controller;
    let agents = controller.agents.get();
    if agents.is_empty() {
        return None;
    }
    let active = controller.active_tab.get();
    let focused = controller
        .tabs
        .get()
        .into_iter()
        .find(|tab| tab.key == active)
        .and_then(|tab| {
            let pane = tab.panes.iter().find(|pane| pane.focused)?.id;
            Some(FocusedPane {
                space_id: controller
                    .spaces
                    .get()
                    .into_iter()
                    .find(|space| space.selected)
                    .map(|space| space.id),
                tab_key: tab.key.clone(),
                pane_id: pane,
            })
        });
    let jump = actions.clone();
    let on_agent: PressAgent<S> = Rc::new(move |_state: &mut S, agent: &AgentEntry| {
        jump.commit_agent(&after, agent);
    });
    let data = AgentsData {
        agents: &agents,
        focused,
    };
    Some(agents_section(&data, &on_agent))
}

/// The tree over the controller's tracked signals — the [`TreeData`] borrow
/// [`tree_body`] renders from — in `mode`, committing with `after`. A free fn
/// since it borrows four owned locals across the call.
pub(super) fn tree_view<S: 'static>(
    actions: &Actions,
    mode: TreeMode,
    after: AfterCommit,
) -> impl View<S> + use<S> {
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
    let docked = mode == TreeMode::Docked;
    tree_body(&data, &tree_actions(actions, after, docked), mode)
}

/// Wire the tree's [`TreeActions`]/[`TreeMutations`] onto this screen's
/// existing dialog-opener/commit/new-pane handlers — every gesture the tree
/// can fire, bridged from its view-only callbacks to whichever state mounts
/// it (`AppState` under the sheet, a page of the root overlay host, and
/// `TerminalState` in the rail); only the rail gets overflow menus.
fn tree_actions<S: 'static>(actions: &Actions, after: AfterCommit, docked: bool) -> TreeActions<S> {
    let read_only = actions.connection.is_read_only.get();

    let toggle_space_actions = actions.clone();
    let toggle_tab_actions = actions.clone();
    let commit_actions = actions.clone();

    let mutations = (!read_only).then(|| {
        let rename_space_actions = actions.clone();
        let rename_tab_actions = actions.clone();
        let rename_pane_actions = actions.clone();
        let new_space_actions = actions.clone();
        let new_tab_actions = actions.clone();
        let new_pane_actions = actions.clone();
        let menu_actions = actions.clone();
        TreeMutations {
            on_rename_space: Rc::new(move |_app: &mut S, id: &str, label: &str| {
                rename_space_actions.open_rename_space_dialog(id, label);
            }),
            on_rename_tab: Rc::new(move |_app: &mut S, tab_id: u64, label: &str| {
                rename_tab_actions.open_rename_tab_dialog(tab_id, label);
            }),
            on_rename_pane: Rc::new(
                move |_app: &mut S, pane_id: u32, is_plugin: bool, title: &str| {
                    rename_pane_actions.open_rename_pane_dialog(pane_id, is_plugin, title);
                },
            ),
            on_new_space: Rc::new(move |_app: &mut S| {
                new_space_actions.open_create_space_dialog();
            }),
            // Both create rows carry the level they were drawn under, and both
            // move the session there before creating: a create always lands
            // where the connection is, so targeting is a switch, not a flag.
            on_new_tab: Rc::new(move |_app: &mut S, space: Option<&str>| {
                new_tab_actions.open_new_tab_dialog_in(space);
            }),
            on_new_pane: Rc::new(move |_app: &mut S, path: &TabPath| {
                new_pane_actions.new_tree_pane(path);
            }),
            on_menu: docked.then(|| {
                Rc::new(move |_app: &mut S, anchor: Rect, target: &RowTarget| {
                    menu_actions.open_row_menu(anchor, target);
                }) as PressMenu<S>
            }),
        }
    });

    TreeActions {
        on_space: Rc::new(move |_app: &mut S, id: &str, expanded: bool| {
            toggle_space_actions.toggle_tree_space(id, expanded);
        }),
        on_tab: Rc::new(move |_app: &mut S, key: &str, expanded: bool| {
            toggle_tab_actions.toggle_tree_tab(key, expanded);
        }),
        on_pane: Rc::new(
            move |_app: &mut S, path: &TabPath, pane_id: u32, is_plugin: bool| {
                commit_actions.commit_tree_pane(&after, path, pane_id, is_plugin);
            },
        ),
        mutations,
    }
}

/// The side sheet's sticky footer: the hold-to-edit discoverability tip (see
/// [`FOOTER_TIP`]) first, then one icon-only row for the two session actions
/// the overflow menu used to carry (rename/detach) — icons rather than two
/// full-width labelled rows, so the footer's own share of the sheet stays
/// small against the tree above it.
///
/// Both presses are gated on [`TerminalController::signals_alive`], for the
/// reason [`sheet_tree_body`] takes its own liveness check: this is a page of
/// the **root** overlay host and outlives the screen that pushed it, so an
/// orphaned footer's rename would seed itself off a disposed signal and its
/// detach would pop a stack no longer carrying `/terminal`. Quiet no-ops
/// instead — the tree above them has already emptied itself.
fn sheet_footer(actions: &Actions, sheet: &SheetHandle) -> impl View<AppState> + use<> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);

    let mut tip_style = sized(&theme.type_scale.body_small, FOOTER_TIP_SIZE, colors.fg_dim);
    tip_style.style = FontStyle::Italic;

    let rename = {
        let actions = actions.clone();
        let sheet = sheet.clone();
        footer_action(
            icons::EDIT,
            FOOTER_RENAME_LABEL,
            &colors,
            move |_app: &mut AppState| {
                if !actions.controller.signals_alive() {
                    return;
                }
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
                if !actions.controller.signals_alive() {
                    return;
                }
                // Detach pops the **route** stack, a different navigator than
                // this sheet's overlay host — a plain `close` (not `close_then`)
                // carries no ordering hazard, the same shape Attach takes there.
                sheet.close();
                actions.detach();
            },
        )
    };

    column()
        .child(text(FOOTER_TIP).style(tip_style))
        .child(v_gap(spacing::SM))
        .child(centered_row(vec![
            inflexible(rename),
            h_gap(spacing::MD),
            inflexible(detach),
        ]))
        .cross_axis(CrossAxisAlignment::Stretch)
}

/// The footer icon buttons' mark size, in logical px — `connect`'s saved-server
/// delete affordance's own size, the app's other bare-icon press target.
const FOOTER_ICON_SIZE: f64 = 18.0;
/// The footer icon buttons' square tap target — the platform-minimum touch
/// size, same as that delete affordance's own box.
const FOOTER_TARGET: f64 = 44.0;
/// The footer's tip text size.
const FOOTER_TIP_SIZE: f32 = 10.0;

/// One footer action: `crate::ui::topbar::icon_button`, the app's one vector
/// press-target primitive — dim at rest, washed by its own built-in pressed
/// state, and labelled `label` for accessibility (never a font glyph —
/// `crate::ui::topbar`'s chrome-icon rule).
fn footer_action(
    mark: impl Into<IconData>,
    label: &'static str,
    colors: &Palette,
    on_press: impl Fn(&mut AppState) + 'static,
) -> impl View<AppState> {
    icon_button(mark, label, on_press)
        .ink(colors.fg_dim)
        .icon_size(FOOTER_ICON_SIZE)
        .box_size(Size::new(FOOTER_TARGET, FOOTER_TARGET))
        .radius(FOOTER_TARGET / 2.0)
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
            has_bell: false,
            floating_panes_visible: false,
            fullscreen_active: false,
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
            agent: None,
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
            has_bell: false,
            floating_panes_visible: false,
            fullscreen_active: false,
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
        let mut tab = tab_with(vec![tiled, floating]);
        tab.floating_panes_visible = true;
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
        let mut tab = tab_with(vec![tiled, floating]);
        tab.floating_panes_visible = true;
        assert_eq!(
            resolve_tapped_pane(&tab, 5, 5).map(|p| p.id),
            Some(10),
            "the floating pane exists but does not cover this cell, so tiled \
             still answers"
        );
    }

    #[test]
    fn a_hidden_floating_pane_covering_the_cell_is_skipped_for_the_tiled_one_beneath() {
        let mut floating = pane_at(27, 0, 1, 80, 47);
        floating.floating = true;
        let mut tab = tab_with(vec![
            pane_at(10, 0, 0, 40, 24),
            pane_at(11, 40, 0, 40, 24),
            floating,
        ]);
        tab.floating_panes_visible = false;
        assert_eq!(resolve_tapped_pane(&tab, 5, 5).map(|p| p.id), Some(10));
        assert_eq!(resolve_tapped_pane(&tab, 45, 5).map(|p| p.id), Some(11));
    }

    #[test]
    fn a_visible_floating_pane_wins_over_the_tiled_one_beneath() {
        let mut floating = pane_at(27, 0, 1, 80, 47);
        floating.floating = true;
        let mut tab = tab_with(vec![pane_at(10, 0, 0, 40, 24), floating]);
        tab.floating_panes_visible = true;
        assert_eq!(resolve_tapped_pane(&tab, 5, 5).map(|p| p.id), Some(27));
    }

    // `resolve_pinch_target` — which pane a pinch toggles.

    fn two_panes() -> TabEntry {
        let mut left = pane_at(10, 0, 0, 40, 24);
        left.focused = true;
        tab_with(vec![left, pane_at(11, 40, 0, 40, 24)])
    }

    #[test]
    fn a_spread_over_a_pane_targets_that_pane() {
        let tab = two_panes();
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::EnterFullscreen, 45, 5),
            Some((11, false))
        );
    }

    #[test]
    fn a_spread_in_a_gutter_targets_the_focused_pane() {
        let tab = two_panes();
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::EnterFullscreen, 90, 30),
            Some((10, false))
        );
    }

    #[test]
    fn a_spread_while_a_pane_is_fullscreen_is_a_no_op() {
        let mut tab = two_panes();
        tab.panes[1].is_fullscreen = true;
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::EnterFullscreen, 5, 5),
            None
        );
    }

    #[test]
    fn a_converge_targets_the_focused_pane_and_is_a_no_op_without_fullscreen() {
        let mut tab = two_panes();
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::ExitFullscreen, 5, 5),
            None
        );
        tab.panes[0].focused = false;
        tab.panes[1].focused = true;
        tab.panes[1].is_fullscreen = true;
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::ExitFullscreen, 5, 5),
            Some((11, false))
        );
    }

    #[test]
    fn a_converge_with_two_panes_flagged_fullscreen_targets_the_focused_tiled_one() {
        let mut tab = two_panes();
        tab.panes[0].is_fullscreen = true;
        tab.panes[0].focused = false;
        tab.panes[1].is_fullscreen = true;
        tab.panes[1].focused = true;
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::ExitFullscreen, 5, 5),
            Some((11, false))
        );
    }

    #[test]
    fn a_converge_with_a_focused_floating_pane_falls_back_to_the_flagged_tiled_pane() {
        let mut tab = two_panes();
        tab.fullscreen_active = true;
        tab.panes[0].focused = false;
        tab.panes[1].is_fullscreen = true;
        let mut floating = pane_at(99, 0, 0, 80, 24);
        floating.floating = true;
        floating.focused = true;
        tab.panes.push(floating);
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::ExitFullscreen, 5, 5),
            Some((11, false))
        );
    }

    #[test]
    fn a_spread_under_a_hidden_floating_pane_targets_the_tiled_pane_beneath() {
        let mut tab = two_panes();
        let mut floating = pane_at(27, 0, 0, 80, 24);
        floating.floating = true;
        tab.panes.push(floating);
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::EnterFullscreen, 45, 5),
            Some((11, false))
        );
    }

    #[test]
    fn a_spread_never_targets_a_visible_floating_pane() {
        let mut tab = two_panes();
        let mut floating = pane_at(27, 0, 0, 80, 24);
        floating.floating = true;
        floating.focused = true;
        floating.cols = 100;
        floating.rows = 40;
        tab.panes.push(floating);
        tab.floating_panes_visible = true;
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::EnterFullscreen, 45, 5),
            Some((11, false)),
            "the tiled pane under the focal, not the floating one over it"
        );
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::EnterFullscreen, 90, 30),
            Some((10, false)),
            "a gutter focal under the floating pane falls back to the focused tiled pane"
        );
    }

    #[test]
    fn a_spread_with_the_tab_flag_but_no_pane_flag_is_a_no_op() {
        let mut tab = two_panes();
        tab.fullscreen_active = true;
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::EnterFullscreen, 5, 5),
            None
        );
    }

    #[test]
    fn a_converge_with_the_tab_flag_but_no_pane_flag_targets_the_focused_tiled_pane() {
        let mut tab = two_panes();
        tab.fullscreen_active = true;
        assert_eq!(
            resolve_pinch_target(&tab, term::PinchIntent::ExitFullscreen, 5, 5),
            Some((10, false))
        );
    }

    // `resolve_tap_action` — adds the already-focused early-out.

    #[test]
    fn a_tap_under_a_hidden_floating_pane_focuses_the_tiled_pane_beneath() {
        let mut floating = pane_at(27, 0, 0, 80, 24);
        floating.floating = true;
        let mut left = pane_at(10, 0, 0, 40, 24);
        left.focused = true;
        let mut tab = tab_with(vec![left, pane_at(11, 40, 0, 40, 24), floating]);
        tab.floating_panes_visible = false;
        assert_eq!(resolve_tap_action(&tab, 45, 5), Some((11, false)));

        tab.floating_panes_visible = true;
        assert_eq!(
            resolve_tap_action(&tab, 45, 5),
            Some((27, false)),
            "with the layer visible the floating pane takes the tap"
        );
        tab.panes[2].focused = true;
        tab.panes[0].focused = false;
        assert_eq!(
            resolve_tap_action(&tab, 45, 5),
            None,
            "already the focused pane"
        );
    }

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
