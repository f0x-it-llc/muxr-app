//! The `/terminal` route page: [`terminal_page`] hosts a [`TerminalController`] and arranges
//! [`super::widgets`] around it. The app bar's menu press reaches [`open_terminal_menu`]; on a
//! Tablet the tree docks beside the card.

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

/// `setTimeout(closeSheet, 260)` in the mock: the selection visibly moves before the sheet closes.
pub(super) const SHEET_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(260);

const SHEET_TITLE: &str = "panes & tabs";
const FOOTER_RENAME_LABEL: &str = "Rename session";
const FOOTER_DETACH_LABEL: &str = "Detach session";
const FOOTER_TIP: &str = "hold a row to rename";

const RENAME_TITLE: &str = "Rename session";
const CREATE_SPACE_TITLE: &str = "New space";
const RENAME_SPACE_TITLE: &str = "Rename space";
const RENAME_TAB_TITLE: &str = "Rename tab";
const RENAME_PANE_TITLE: &str = "Rename pane";
const DETACHED_TOAST: &str = "detached";
/// The sheet has no disabled row state, so a row a read-only credential may not use says this.
const READ_ONLY_TOAST: &str = "read-only session";

/// A gutter the stage animates to zero with `WRAP_PAD`, not a wider `WRAP_PAD`.
const WIDE_WRAP_PAD_PX: f64 = 22.0;

/// Top-only; the sides keep [`WRAP_PAD`] and fullscreen takes neither.
const PORTRAIT_TOP_GAP_PX: f64 = WIDE_WRAP_PAD_PX - WRAP_PAD;

const _: () = {
    assert!(WIDE_WRAP_PAD_PX > WRAP_PAD);
    assert!(PORTRAIT_TOP_GAP_PX > 0.0);
};

/// A `Component` under [`AppState`], so its `init` runs under a real reactive `Owner`.
pub fn terminal_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    component(TerminalScreen { overlays })
}

pub struct TerminalScreen {
    overlays: NavigatorController<AppState>,
}

/// `live`'s `Rc`-backed seams cannot live on the controller, so they live here.
pub struct TerminalState {
    pub actions: Actions,
    /// `Some` from the first `build` that observes `TerminalController::live`, never `None` again.
    pub(super) live: Option<LiveSession>,
}

/// One cloneable bundle: overlay pages get `&mut AppState` and cannot see [`TerminalState`].
/// Published in a `thread_local!` for the app bar, since `provide_context` needs `Send + Sync`.
#[derive(Clone)]
pub struct Actions {
    id: u64,
    pub overlays: NavigatorController<AppState>,
    pub app_nav: Nav,
    pub controller: Arc<TerminalController>,
    pub connection: Arc<ConnectionController>,
    /// Shared by every clone, so one mount leaves `/terminal` at most once.
    detaching: Rc<Cell<bool>>,
    /// A shared cell: `Actions` is cloned into [`TERMINAL_CHROME`] before any `LiveSession` exists.
    live_probe: Rc<RefCell<Option<LiveProbe>>>,
    grid_hooks: Rc<RefCell<Option<GridHooks>>>,
    tablet: Rc<Cell<bool>>,
    menu_anchor: Rc<Cell<Rect>>,
}

#[derive(Clone)]
pub struct LiveProbe {
    pub emulator: term::SharedEmulator,
    /// `None` on a real `muxrd` session.
    pub feed: Option<Rc<RefCell<DemoFeed>>>,
}

pub fn debug_live_probe() -> Option<LiveProbe> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|actions| actions.live_probe.borrow().clone())
    })
}

#[derive(Clone, Default)]
pub struct GridHooks {
    /// `None` on herdr, whose single-pane stream has nothing a tap could focus.
    pub on_content_tap: Option<term::ContentTapSink>,
    pub on_wheel: Option<term::WheelSink>,
    /// `None` on herdr and on a read-only credential; see [`resolve_pinch_target`].
    pub on_pinch: Option<term::PinchSink>,
}

pub fn debug_grid_hooks() -> Option<GridHooks> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|actions| actions.grid_hooks.borrow().clone())
    })
}

thread_local! {
    static DEMO_COPY_SINK: RefCell<Option<term::CopySink>> = const { RefCell::new(None) };
}

pub fn set_demo_copy_sink(sink: Option<term::CopySink>) {
    DEMO_COPY_SINK.with(|cell| *cell.borrow_mut() = sink);
}

fn demo_copy_sink() -> Option<term::CopySink> {
    DEMO_COPY_SINK.with(|cell| cell.borrow().clone())
}

thread_local! {
    /// Tagged with the mount's [`Actions::id`] so a cleanup withdraws only its own entry: an
    /// incoming page builds before the outgoing one tears down.
    static TERMINAL_CHROME: RefCell<Option<Actions>> = const { RefCell::new(None) };
}

static NEXT_CHROME_ID: AtomicU64 = AtomicU64::new(1);

/// Not `#[cfg(test)]`: the integration test binaries read it.
pub fn debug_controller() -> Option<Arc<TerminalController>> {
    TERMINAL_CHROME.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|actions| Arc::clone(&actions.controller))
    })
}

/// A press with no terminal screen mounted is ignored.
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
        // App-scoped handles resolve here, never in a press handler.
        let app_nav = app_nav();
        // Read in `init`, the one moment `Nav::param` is exact.
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
        let controller = use_controller::<TerminalController, AppFailure>(move || {
            TerminalController::new(
                expect_terminal_repository(),
                expect_sessions_repository(),
                expect_preferences_repository(),
                session.clone(),
            )
        });

        controller.set_attach_target_in_space(space, tab, pane);

        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        // The one mock-vs-live decision: a session id with a live client mounts the grid from the start.
        let connection = expect_connection_controller();
        if controller.session_id().is_some() && connection.client().is_some() {
            controller.attach_live();
        }

        let tablet = Responsive::of(use_context::<WindowMetrics>()).is_tablet();
        controller.set_docked(tablet);

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
        TERMINAL_CHROME.with(|cell| *cell.borrow_mut() = Some(actions.clone()));

        // `on_cleanup` needs `Send + Sync`, which the `Rc`-backed `Nav` cannot satisfy.
        let bar_fullscreen = actions.app_nav.fullscreen();
        let id = actions.id;
        on_cleanup(move || {
            // Leaving fullscreen set would strand a hidden bar on the route below.
            bar_fullscreen.set(false);
            // Every non-collapse exit unmounts here; unconditional costs only a no-op poll.
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

    /// [`LiveSession`] is built once, lazily, here: `build` runs under the screen's `Owner`, which
    /// binds the pump's `on_cleanup`; a press handler has no owner on desktop.
    fn build(&self, state: &mut TerminalState) -> impl View<TerminalState> {
        let controller = Arc::clone(&state.actions.controller);

        // Tracked, so whichever entry flipped `live` wakes this rebuild.
        if controller.live.get() && state.live.is_none() {
            let session = LiveSession::enter(&controller, &state.actions.connection);
            *state.actions.live_probe.borrow_mut() = Some(LiveProbe {
                emulator: Rc::clone(&session.emulator),
                feed: session.feed.demo().map(Rc::clone),
            });

            let on_content_tap: Option<term::ContentTapSink> = if controller.is_herdr_session() {
                None
            } else {
                let controller = Arc::clone(&controller);
                let missed = Rc::new(Cell::new(false));
                let sink: term::ContentTapSink = Rc::new(move |col: u32, row: u32| {
                    let resolved = controller
                        .active_tab_entry()
                        .and_then(|tab| resolve_tap_action(&tab, col, row));
                    let Some((pane_id, is_plugin)) = resolved else {
                        // An empty or stale layout snapshot misses every tap, so the first miss of a mount re-reads
                        // the layout once.
                        if !missed.replace(true) {
                            let controller = Arc::clone(&controller);
                            spawn_local(async move { controller.reload_tabs().await });
                        }
                        // Nothing to focus: the tap is on the current pane, so raise the keyboard.
                        return true;
                    };
                    let controller = Arc::clone(&controller);
                    spawn_local(async move {
                        controller.focus_pane_from_tap(pane_id, is_plugin).await;
                    });
                    // A focus-moving tap is pointing, not typing: no keyboard.
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
            // Read-only is fixed for the mount, so this untracked read is the whole gate.
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
                        frust::perform_haptic(frust::HapticFeedback::Selection);
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
        // The app-side `SIGWINCH`: forward the size the previous layout recorded, if it changed.
        if let Some(session) = state.live.as_ref()
            && let Some((cols, rows)) = session.emulator.borrow_mut().take_pending_resize()
        {
            session.resize(cols, rows);
        }

        let lines = controller.lines.get();
        let epoch = controller.reveal_epoch.get();
        let fullscreen = controller.fullscreen.get();
        let tabs = controller.tabs.get();
        let active = controller.active_tab.get();
        let title = active_title(&tabs, &active);
        let link = controller.link.get();
        // Gates every control read-only refuses and puts the grid in its no-byte read-only mode.
        let read_only = state.actions.connection.is_read_only.get();

        let theme = active_theme();
        let colors = Palette::from_theme(&theme);
        let ink = CardInk::from_theme(&theme);

        // The gutters go to the stage, which animates them to zero itself.
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

        let body = match state.live.as_ref() {
            Some(session) => {
                // Tracked: the pump's one write per wake repaints the live pane.
                let generation = session.generation.get();
                let mut grid = term::terminal_grid(
                    Rc::clone(&session.emulator),
                    generation,
                    term::terminal_palette(&theme),
                    controller.font_size.get(),
                )
                .cursor(controller.cursor_style.get())
                .font_family(controller.font_family.get())
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

        let crumbs = if tablet && responsive.is_full() {
            controller.crumb_labels()
        } else {
            Vec::new()
        };
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

        // Outside the stage, so the bar band never flashes the shell's clear colour.
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

        match state.live.as_ref() {
            Some(session) => {
                let bar = if tablet {
                    let shown = controller.key_bar_visible.get() && !read_only;
                    let fn_row = controller.fn_row_visible;
                    Either::Left(keybar::tablet_key_bar(
                        shown,
                        responsive.band,
                        fn_row,
                        &session.input,
                    ))
                } else {
                    // The keyboard's height is the one IME proxy: this screen has no other text input.
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

fn active_title(tabs: &[TabEntry], active: &str) -> String {
    tabs.iter()
        .find(|tab| tab.key == active)
        .map(|tab| tab.pane_title.clone())
        .unwrap_or_default()
}

/// Fullscreen wins, then floating beats tiled, but only while the floating layer is visible: a
/// hidden floating pane never captures a tap or pinch. The wire carries no z-order, so the
/// topmost pane is the last in wire order.
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

/// The pinch's hit test, blind to the floating layer and to fullscreen.
fn tiled_pane_at(tab: &TabEntry, col: u32, row: u32) -> Option<&PaneEntry> {
    tab.panes
        .iter()
        .filter(|pane| !pane.floating)
        .rfind(|pane| {
            col >= pane.x && col < pane.x + pane.cols && row >= pane.y && row < pane.y + pane.rows
        })
}

/// `Some` focuses without raising the keyboard and `None` raises it, so tapping an unfocused
/// pane twice focuses, then types.
fn resolve_tap_action(tab: &TabEntry, col: u32, row: u32) -> Option<(u32, bool)> {
    let pane = resolve_tapped_pane(tab, col, row)?;
    if pane.focused {
        return None;
    }
    Some((pane.id, pane.is_plugin))
}

/// Spread targets the tiled pane under the focal cell, else the focused tiled pane; converge
/// acts only while fullscreen. `focused` comes before `is_fullscreen`: zellij's `is_focused`
/// is a union over clients, and muxrd makes `focused` this connection's own pane.
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

#[derive(Debug)]
enum WheelRoute {
    /// Gated on the server capability alone; the server picks the pane under the pointer.
    Mouse,
    /// Scrolls the active tab's focused pane, never the pointer's.
    Scroll {
        pane_id: u32,
        is_plugin: bool,
    },
    Drop,
}

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

/// Glyph's bar icon slot (`.ab-icon` 40px) and padding (6px), private to the bar.
const BAR_ICON_SLOT: f64 = 40.0;
const BAR_PAD_X: f64 = 6.0;

fn bar_trailing_slot(metrics: &WindowMetrics) -> Rect {
    let right = metrics.size.width - metrics.insets.view_padding.right - BAR_PAD_X;
    let top = metrics.insets.view_padding.top;
    Rect::new(right - BAR_ICON_SLOT, top, right, top + BAR_HEIGHT)
}

fn toggle_fullscreen(state: &mut TerminalState) {
    state.actions.toggle_fullscreen();
}

/// Read teardown-safely: `None` also means this mount is gone.
fn expanded_space(controller: &TerminalController) -> Option<String> {
    controller.expanded_space.try_get_untracked().flatten()
}

/// Edge-triggered, never build-time: it bumps a generation a shell watcher polls, so a
/// build-time call re-requests every frame.
fn restore_system_ui() {
    set_system_ui_mode(SystemUiMode::EdgeToEdge);
}

impl Actions {
    /// The flag outlives this screen, so `init`'s [`on_cleanup`] clears it on unmount.
    pub fn toggle_fullscreen(&self) {
        let on = self.controller.toggle_fullscreen();
        self.app_nav.set_fullscreen(on);
        if on {
            // Sticky: an edge swipe reveals the bars transiently and they auto-hide.
            set_system_ui_mode(SystemUiMode::ImmersiveSticky);
        } else {
            restore_system_ui();
        }
    }

    /// Refreshes the layout without blocking the open: a slow link costs a late row, never a
    /// stuck sheet. Dismissal resets the whole browse.
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
            // Every read is `try_*`: the screen can unmount across either await, and a plain read of a
            // disposed signal panics.
            if expanded_space(&controller).as_deref() != Some(id.as_str()) {
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

    pub fn toggle_tree_tab(&self, key: &str, expanded: bool) {
        if expanded {
            self.controller.collapse_tab();
        } else {
            self.controller.expand_tab(key.to_string());
        }
    }

    /// `SwitchSpace` has no read-only gate on the wire, so a read-only credential reaches this; a
    /// server refusal surfaces as a toast.
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

    /// First tap wins at the controller's single-flight claim; latest tap wins inside the close
    /// beat, which re-times the one pending close. Read-only reaches it: neither `GoToTab` nor
    /// `FocusPane` has a read-only gate on the wire.
    pub fn commit_pane(&self, sheet: &SheetHandle, tab_key: &str, pane_id: u32, is_plugin: bool) {
        self.commit_pane_then(
            &AfterCommit::Close(sheet.clone()),
            tab_key,
            pane_id,
            is_plugin,
        );
    }

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

    fn commit_pane_then(&self, after: &AfterCommit, tab_key: &str, pane_id: u32, is_plugin: bool) {
        let controller = Arc::clone(&self.controller);
        let after = after.clone();
        let tab_key = tab_key.to_string();
        spawn_local(async move {
            if !controller.commit_pane(tab_key, pane_id, is_plugin).await {
                // A refused verb toasts on its own; a dropped tap leaves the close to the first.
                return;
            }
            after.land(&controller).await;
        });
    }

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

    /// Re-reads the current name rather than capturing it. Renaming mutates, so a read-only
    /// credential is refused here: the row stays and says why.
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

    /// One pop per mount, latched through [`Actions::detaching`]; a refusal re-arms it.
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

    /// A row under a browsed space switches to it first, so the tab lands where the user pointed;
    /// this is also the only way into an empty space.
    pub fn open_new_tab_dialog_in(&self, space_id: Option<&str>) {
        let Some(space_id) = space_id.map(str::to_string) else {
            self.open_new_tab_dialog();
            return;
        };
        let controller = Arc::clone(&self.controller);
        let actions = self.clone();
        spawn_local(async move {
            // Opening behind a refused or dropped switch would create in the wrong space.
            if controller.adopt_space(space_id).await {
                actions.open_new_tab_dialog();
            }
        });
    }

    /// A blank name is legitimate (the server's default naming); cancel creates nothing.
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

    /// Reloads only the tab list: a full load replays the reveal for nothing.
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

    pub(crate) fn new_tree_pane(&self, path: &TabPath) {
        let controller = Arc::clone(&self.controller);
        let (space_id, tab_key) = (path.space_id.clone(), path.tab_key.clone());
        spawn_local(async move {
            controller.new_pane_in(space_id, tab_key, false).await;
        });
    }
}

fn sheet_tree_body(actions: &Actions, sheet: &SheetHandle) -> impl View<AppState> + use<> {
    // A root overlay page can outlive the screen: one untracked liveness check guards the
    // tracked reads below, which panic on a disposed owner.
    if !actions.controller.signals_alive() {
        return Either::Left(stretch_column(Vec::new()));
    }
    let after = AfterCommit::Close(sheet.clone());
    let agents = agents_view(actions, after.clone());
    Either::Right(tree_split(
        tree_view(actions, TreeMode::Sheet, after),
        agents,
    ))
}

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
            // A create lands where the connection is, so targeting is a switch, not a flag.
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

/// Both presses check [`TerminalController::signals_alive`]: this root overlay page outlives
/// the screen, and an orphaned press would read a disposed signal or pop the wrong stack.
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
                // `close_then`: an unchained dialog push lands ahead of the sheet's staged exit.
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
                // Detach pops the route stack, not this overlay host, so a plain close has no ordering hazard.
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

/// The connect screen's delete affordance size.
const FOOTER_ICON_SIZE: f64 = 18.0;
/// The platform-minimum touch target.
const FOOTER_TARGET: f64 = 44.0;
const FOOTER_TIP_SIZE: f32 = 10.0;

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
            tab("btop", "ed@workbench: ~"),
            tab("devbox", "ed@devbox: ~"),
        ];
        assert_eq!(active_title(&tabs, "devbox"), "ed@devbox: ~");
        assert_eq!(active_title(&tabs, "btop"), "ed@workbench: ~");
        assert_eq!(active_title(&tabs, "gone"), "", "no tab, no title");
    }

    #[test]
    fn the_sheet_closes_a_beat_after_a_tab_switch_rather_than_instantly() {
        assert_eq!(SHEET_CLOSE_DELAY.as_millis(), 260);
    }

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
