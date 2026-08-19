//! The "panes & tabs" sheet's body — `docs/terminal.html`'s `#pickerSheet`,
//! top to bottom, minus the handle the scaffold already draws.
//!
//! Free fns over `&Actions` rather than methods on it (the
//! `features::sessions` `sheet_action` precedent): the sheet is a page of the
//! **root overlay host**, so every callback below receives `&mut AppState` and
//! reaches this screen only through the captured bundle.

use std::rc::Rc;

use frust::authoring::{Color, DashPattern};
use frust::{
    Align, Alignment, AnyView, Axis, BorderStyle, EdgeInsets, FlexView, GestureDetector, Get,
    Padding, SizedBox, WindowMetrics, any, container, inflexible, text, use_context,
};

use crate::AppState;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::SheetHandle;
use crate::ui::spacing;

use super::super::chrome::{DASH_OFF, DASH_ON, HAIRLINE, wrap_row};
// A sanctioned pages -> widgets back-edge (see `widgets`' own module docs and
// `docs/FEATURES_ARCHITECTURE.md`'s widgets/ paragraph) — this module is not
// independently composable from `pages` the way most of its siblings are.
use super::super::pages::Actions;
use super::panes::{PANE_ROW_GAP, new_pane_row, pane_item};
use super::pills::{PILL_GAP, PillState, pill};
use super::sheet_bits::{
    SECTION_BOTTOM_GAP, SECTION_TOP_GAP, section_label, sheet_scroll_body, sheet_top_row,
};
use super::util::{column, v_gap};

/// The sheet's three section labels. The third names the tab whose panes it is
/// listing (`panes — devbox`).
const SECTION_SPACES: &str = "spaces";
const SECTION_TABS: &str = "tabs";
const SECTION_PANES: &str = "panes \u{2014} ";

/// The space/tab sections' own create rows: full-size, dashed rows dimensionally
/// consistent with [`new_pane_row`], not the small `.pill.add` they replaced.
/// Content text, not chrome, so these stay runs rather than becoming vectors.
const ADD_ROW_PLUS: &str = "+";
const ADD_SPACE_LABEL: &str = "new space";
const ADD_TAB_LABEL: &str = "new tab";
/// [`new_pane_row`]'s own padding and text size, taken from that row's constants
/// rather than re-spelled, so [`add_row`] cannot drift out of dimensional step.
const ADD_ROW_PAD: f64 = super::panes::NEW_PANE_PAD;
const ADD_ROW_GAP: f64 = spacing::XS;
const ADD_ROW_TEXT_SIZE: f32 = super::pills::PILL_SIZE;

/// [`sheet_scroll_body`]'s height cap: the window's own height minus this estimate
/// of everything outside the scrollable region — `ui/sheet.rs`'s handle zone, panel
/// padding and safe-area inset on one side, this sheet's fixed top row and the gap
/// under it on the other. A generous approximation rather than an exact figure
/// imported from `ui/sheet.rs`'s private geometry: the cost of over-estimating is a
/// slightly shorter scroll band, not a broken one.
const SHEET_SCROLL_RESERVE_PX: f64 = 220.0;
/// The scrollable region's floor, so a short window still leaves a usable,
/// clearly-scrollable band rather than the reserve above collapsing it
/// toward zero.
const SHEET_SCROLL_FLOOR_PX: f64 = 240.0;

/// The "panes & tabs" sheet's body.
///
/// **Browse, then commit.** A tab pill points the sheet at another tab and
/// re-slices the pane list below it (no RPC, no dismissal); a space pill switches
/// this connection's view and reloads behind it (still no dismissal); only a pane
/// row moves the session and closes the sheet.
///
/// **The spaces section is herdr's alone** — zellij has no spaces at all, so the
/// section is *absent* rather than empty: no label, and no gap where one would be.
///
/// **The top row stays fixed; everything under it scrolls**: [`top_row`] is painted
/// outside the scroll region and every section below it handed to
/// [`sheet_scroll_body`], so a session with many tabs/panes stays fully reachable.
///
/// The builder is re-invoked on every rebuild while the sheet is up, so its reads
/// are tracked and a browse is reflected in the pills, the section label *and* the
/// pane list without the sheet being rebuilt by hand.
pub(crate) fn sheet_body(actions: &Actions, sheet: &SheetHandle) -> AnyView<AppState> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    // Tracked reads: browsing a tab, committing a pane or switching a
    // space from inside the sheet re-runs this builder, and a credential
    // turning read-only withdraws the three create affordances below.
    let spaces = actions.controller.spaces.get();
    let tabs = actions.controller.tabs.get();
    let active = actions.controller.active_tab.get();
    let browsed = actions.controller.browsed_tab.get();
    let read_only = actions.connection.is_read_only.get();

    let top = top_row(sheet, &colors);
    let mut children = vec![v_gap(SECTION_TOP_GAP)];

    if actions.controller.has_spaces() {
        children.push(section_label(SECTION_SPACES, &colors));
        children.push(v_gap(SECTION_BOTTOM_GAP));
        let space_pills: Vec<AnyView<AppState>> = spaces
            .iter()
            .map(|space| {
                let tap_actions = actions.clone();
                let id = space.id.clone();
                let edit_actions = actions.clone();
                let edit_id = space.id.clone();
                let edit_label = space.name.clone();
                pill(
                    space.name.clone(),
                    if space.selected {
                        PillState::Selected
                    } else {
                        PillState::Idle
                    },
                    &colors,
                    move |_app: &mut AppState| tap_actions.select_space(&id),
                    (!read_only).then(|| {
                        Rc::new(move |_app: &mut AppState| {
                            edit_actions.open_rename_space_dialog(&edit_id, &edit_label)
                        }) as Rc<dyn Fn(&mut AppState)>
                    }),
                )
            })
            .collect();
        children.push(wrap_row(space_pills, PILL_GAP));
        // The full-size create row, separate from the pill wrap above it —
        // see `add_row`'s own doc for why it is not one of the wrapped pills.
        if !read_only {
            children.push(v_gap(PANE_ROW_GAP));
            let create_actions = actions.clone();
            children.push(add_row(
                ADD_SPACE_LABEL,
                &colors,
                move |_app: &mut AppState| create_actions.open_create_space_dialog(),
            ));
        }
        // The section's own trailing gap, so the tabs label below sits the
        // same distance from whatever precedes it either way.
        children.push(v_gap(SECTION_TOP_GAP));
    }

    children.push(section_label(SECTION_TABS, &colors));
    children.push(v_gap(SECTION_BOTTOM_GAP));
    let tab_pills: Vec<AnyView<AppState>> = tabs
        .iter()
        .map(|tab| {
            let tap_actions = actions.clone();
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
            let edit_actions = actions.clone();
            let tab_id = tab.tab_id;
            let edit_label = tab.label.clone();
            pill(
                tab.pill_label(),
                state,
                &colors,
                move |_app: &mut AppState| tap_actions.browse_tab(&key),
                (!read_only).then(|| {
                    Rc::new(move |_app: &mut AppState| {
                        edit_actions.open_rename_tab_dialog(tab_id, &edit_label)
                    }) as Rc<dyn Fn(&mut AppState)>
                }),
            )
        })
        .collect();
    children.push(wrap_row(tab_pills, PILL_GAP));
    if !read_only {
        children.push(v_gap(PANE_ROW_GAP));
        let create_actions = actions.clone();
        children.push(add_row(
            ADD_TAB_LABEL,
            &colors,
            move |_app: &mut AppState| create_actions.open_new_tab_dialog(),
        ));
    }

    children.push(v_gap(SECTION_TOP_GAP));
    children.push(section_label(format!("{SECTION_PANES}{browsed}"), &colors));
    children.push(v_gap(SECTION_BOTTOM_GAP));
    if let Some(tab) = tabs.iter().find(|tab| tab.key == browsed) {
        // A tab the sheet is only *browsing* shows no badge at all: its
        // panes carry whatever the last load recorded, and the session is
        // not in any of them.
        let on_session = tab.key == active;
        for pane in &tab.panes {
            let tap_actions = actions.clone();
            let sheet = sheet.clone();
            let key = tab.key.clone();
            let (id, is_plugin) = (pane.id, pane.is_plugin);
            let edit_actions = actions.clone();
            let edit_title = pane.title.clone();
            children.push(pane_item(
                pane,
                on_session && pane.focused,
                &colors,
                move |_app: &mut AppState| tap_actions.commit_pane(&sheet, &key, id, is_plugin),
                (!read_only).then(|| {
                    Rc::new(move |_app: &mut AppState| {
                        edit_actions.open_rename_pane_dialog(id, is_plugin, &edit_title)
                    }) as Rc<dyn Fn(&mut AppState)>
                }),
            ));
            children.push(v_gap(PANE_ROW_GAP));
        }
    }
    if !read_only {
        let tap_actions = actions.clone();
        let hold_actions = actions.clone();
        children.push(new_pane_row(
            &colors,
            move |_app: &mut AppState| tap_actions.new_pane(false),
            move |_app: &mut AppState| hold_actions.new_pane(true),
        ));
    }

    // The window's own height, read once per build like every other responsive
    // value (`crate::responsive`) — `None` in every headless suite, which is what
    // keeps the scroll cap from touching any existing test's geometry.
    let scroll_max_h = use_context::<WindowMetrics>()
        .map(|metrics| (metrics.size.height - SHEET_SCROLL_RESERVE_PX).max(SHEET_SCROLL_FLOOR_PX));
    let scroll_body = sheet_scroll_body(children, scroll_max_h);

    any(column(vec![top, scroll_body]))
}

/// The sheet's top row, whose close button pops it — through the same one
/// gate a pane row's delayed close uses, so pressing it mid-beat closes the
/// sheet once and leaves the route alone.
fn top_row(sheet: &SheetHandle, colors: &Palette) -> AnyView<AppState> {
    let sheet = sheet.clone();
    sheet_top_row(colors, move |_app: &mut AppState| {
        sheet.close();
    })
}

/// The spaces/tabs sections' own create row: full-size and dashed, dimensionally
/// consistent with [`new_pane_row`] — the small `.pill.add` it replaced read as an
/// afterthought beside the pane section's full-width row below it. `label` is the
/// dim run following the amber "+"; it never carries an edit icon, since naming
/// happens in the dialog the add-tab row opens. Declared here rather than beside
/// `new_pane_row`, since only this sheet calls it.
fn add_row<F: Fn(&mut AppState) + 'static>(
    label: &'static str,
    colors: &Palette,
    on_tap: F,
) -> AnyView<AppState> {
    let theme = active_theme();
    let mut plus_style = theme.type_scale.body_small.clone();
    plus_style.size = ADD_ROW_TEXT_SIZE;
    plus_style.color = colors.amber;
    let mut label_style = theme.type_scale.body_small.clone();
    label_style.size = ADD_ROW_TEXT_SIZE;
    label_style.color = colors.fg_dim;

    let row = FlexView::new(
        Axis::Horizontal,
        vec![
            inflexible(text(ADD_ROW_PLUS).style(plus_style)),
            inflexible(SizedBox::<AppState>(Some(ADD_ROW_GAP), None)),
            inflexible(text(label).style(label_style)),
        ],
    );
    any(GestureDetector(
        container(Padding(
            EdgeInsets::all(ADD_ROW_PAD),
            Align(Alignment::CENTER, row),
        ))
        .fill(Color::TRANSPARENT)
        .radius(theme.shape.small)
        .border(colors.outline, HAIRLINE)
        .border_style(BorderStyle::Dashed(DashPattern::new(DASH_ON, DASH_OFF))),
    )
    .on_tap(on_tap))
}
