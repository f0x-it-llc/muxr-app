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
//! the menu, the sheet and the rename dialog are pages on the *app's*
//! navigator, so their callbacks receive `&mut AppState` and cannot see
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

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::authoring::Rect;
use frust::glyph::{MenuEntry, menu_item, menu_separator, show_glyph_menu};
use frust::{
    AnyView, Axis, Component, CrossAxisAlignment, FlexView, Get, GetUntracked, NavigatorController,
    PopResult, any, component, flexible, inflexible, on_cleanup, spawn_local,
};

use crate::AppState;
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::terminal::di::expect_terminal_repository;
use crate::features::terminal::domain::entities::TabEntry;
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::name_input_dialog;
use crate::ui::sheet::{close_sheet, show_sheet};
use crate::ui::toasts::app_toasts;

use super::chrome::term_box;
use super::controller::TerminalController;
use super::pieces::{
    self, PANE_ROW_GAP, PILL_GAP, Palette, PillState, SECTION_BOTTOM_GAP, SECTION_TOP_GAP,
};

/// How long the sheet stays up after a tab pill is tapped, so the selection
/// visibly moves before it closes — `setTimeout(closeSheet, 260)` in the mock.
const SHEET_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(260);

/// The overflow menu's three rows, in order. Indices into the entry list
/// [`show_glyph_menu`] hands back, so they are named once rather than spelled
/// at the match site.
const MENU_PANES: &str = "Panes & tabs";
const MENU_RENAME: &str = "Rename session";
const MENU_DETACH: &str = "Detach session";
const MENU_PANES_INDEX: usize = 0;
const MENU_RENAME_INDEX: usize = 2;
const MENU_DETACH_INDEX: usize = 3;

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

/// The Terminal screen's route entry point: a `Component` under the outer
/// [`AppState`], so its `init` runs under a real reactive `Owner` that
/// disposes the hosted controller on unmount.
pub fn terminal_page(controller: NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(TerminalScreen { nav: controller }))
}

/// The Terminal screen `Component`. Stateless configuration — everything
/// mutable lives in [`TerminalState`] and the controller it hosts.
pub struct TerminalScreen {
    /// The app's one navigator, handed in by the route builder.
    nav: NavigatorController<AppState>,
}

/// The Terminal screen's retained state: the action bundle, and nothing else.
///
/// The [state-split rule](crate::features::terminal::presentation::controller)
/// in practice — the card's animation drivers are inside their own retained
/// widgets, the observed state is in the controller's signals, and what is
/// left here is the handles a press handler reads.
pub struct TerminalState {
    /// Everything this screen can *do*, and the handles it needs to do it.
    pub actions: Actions,
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
    /// The app's navigator — every page *builder* this screen pushes (the
    /// menu, the sheet, the rename dialog) goes through it.
    pub nav: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how this screen pops back to
    /// `/sessions`, and how it publishes the bar's fullscreen flag.
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<TerminalController>,
    /// Where the app bar's trailing `⋮` slot is painting, in window
    /// coordinates — written by `super::chrome::terminal_stage` on every
    /// layout, read when the menu opens. See [`Actions::open_menu`].
    pub anchor: Rc<Cell<Rect>>,
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
        // The screen-scoped view model, over the two app-scoped repositories
        // `di` published. Auto-disposed on unmount via `on_cleanup`.
        let controller = use_controller::<TerminalController, AppFailure>(move || {
            TerminalController::new(
                expect_terminal_repository(),
                expect_sessions_repository(),
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
            nav: self.nav.clone(),
            app_nav,
            controller,
            anchor: Rc::new(Cell::new(Rect::ZERO)),
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

        TerminalState { actions }
    }

    fn build(&self, state: &mut TerminalState) -> AnyView<TerminalState> {
        let controller = &state.actions.controller;
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

        let card = FlexView::new(
            Axis::Vertical,
            vec![
                inflexible(pieces::status_row(
                    &title,
                    fullscreen,
                    &colors,
                    toggle_fullscreen,
                )),
                flexible(1, pieces::card_body(lines, epoch, &colors)),
            ],
        )
        .cross_axis(CrossAxisAlignment::Stretch);

        // The page tone stays **outside** the stage so it fills the whole
        // surface, bar band included: the bar slides in over it, and a page
        // that stopped at the clearance would flash the shell's own clear
        // colour through the gap for the length of that entrance.
        any(term_box(
            super::chrome::terminal_stage(
                card,
                fullscreen,
                colors.card,
                colors.card_border,
                Rc::clone(&state.actions.anchor),
            ),
            colors.void,
            0.0,
        ))
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
    /// The menu is a transparent navigator push, so the bar dims and goes
    /// inert while it is open (`BarState::overlay_active`) — the app's modal
    /// convention, which also means a tap anywhere outside the panel (the
    /// dimmed bar included) dismisses it.
    ///
    /// It is accounted like the app's own overlays
    /// ([`Nav::begin_overlay`](crate::routes::Nav::begin_overlay)/
    /// [`end_overlay`](crate::routes::Nav::end_overlay)) even though it is
    /// pushed by a **frust** helper: this call site is app code, and so is the
    /// `on_result` below. What it cannot have is the *synchronous* half — a
    /// menu dismissal is popped from inside `frust::glyph::menu`'s own paint,
    /// with no app seam in between — so the bar lights up on the completion
    /// callback rather than a rebuild earlier (see
    /// [`Nav::dismiss_overlay`](crate::routes::Nav::dismiss_overlay)).
    pub fn open_menu(&self) {
        let actions = self.clone();
        let done_app_nav = self.app_nav.clone();
        self.app_nav.begin_overlay();
        show_glyph_menu(
            &self.nav,
            self.anchor.get(),
            menu_entries,
            move |_app: &mut AppState, result: PopResult| {
                // Before the arms below: each of the first two opens an
                // overlay of its own, and this one is gone.
                done_app_nav.end_overlay();
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
    /// tab switch is reflected in the pills *and* in the section label without
    /// the sheet being rebuilt by hand.
    pub fn open_sheet(&self) {
        let actions = self.clone();
        show_sheet(&self.app_nav, &self.nav, move || actions.sheet_body());
    }

    /// Switch the card to `key` and close the sheet a beat later, so the
    /// selection visibly moves first (`setTimeout(closeSheet, 260)`).
    pub fn switch_tab(&self, key: &str) {
        let controller = Arc::clone(&self.controller);
        let nav = self.nav.clone();
        let app_nav = self.app_nav.clone();
        let key = key.to_string();
        spawn_local(async move {
            controller.switch_tab(key).await;
            clean_signals::time::sleep(SHEET_CLOSE_DELAY).await;
            close_sheet(&app_nav, &nav);
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
            &self.app_nav,
            &self.nav,
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
    pub fn detach(&self) {
        let controller = Arc::clone(&self.controller);
        let nav = self.app_nav.clone();
        spawn_local(async move {
            if controller.detach().await {
                nav.pop();
                app_toasts().show(DETACHED_TOAST);
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
    fn sheet_body(&self) -> AnyView<AppState> {
        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);
        // Tracked reads: switching a tab from inside the sheet re-runs this
        // builder and moves the selection.
        let spaces = self.controller.spaces.get();
        let tabs = self.controller.tabs.get();
        let active = self.controller.active_tab.get();

        let mut children = vec![self.top_row(&colors), pieces::v_gap(SECTION_TOP_GAP)];

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
                let key = tab.key.clone();
                pieces::pill(
                    tab.pill_label(),
                    if tab.key == active {
                        PillState::Selected
                    } else {
                        PillState::Idle
                    },
                    &colors,
                    move |_app: &mut AppState| actions.switch_tab(&key),
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

    /// The sheet's top row, whose close button pops it.
    fn top_row(&self, colors: &Palette) -> AnyView<AppState> {
        let nav = self.nav.clone();
        let app_nav = self.app_nav.clone();
        pieces::sheet_top_row(colors, move |_app: &mut AppState| {
            close_sheet(&app_nav, &nav)
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
fn menu_entries() -> Vec<MenuEntry> {
    vec![
        menu_item(MENU_PANES),
        menu_separator(),
        menu_item(MENU_RENAME),
        menu_item(MENU_DETACH),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_is_three_rows_with_one_separator_between_the_first_and_the_rest() {
        let entries = menu_entries();
        assert_eq!(entries.len(), 4, "three rows plus the `.dd-sep`");
        assert!(matches!(entries[1], MenuEntry::Separator));
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
        // list, separator included — an off-by-one here would silently run the
        // wrong action.
        let entries = menu_entries();
        for (index, label) in [
            (MENU_PANES_INDEX, MENU_PANES),
            (MENU_RENAME_INDEX, MENU_RENAME),
            (MENU_DETACH_INDEX, MENU_DETACH),
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
