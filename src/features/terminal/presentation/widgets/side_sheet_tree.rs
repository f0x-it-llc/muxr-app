//! The side sheet's body: the spaces → tabs → panes containment tree.
//!
//! Three shapes, one per level — a rounded space **card**, a small tab **rect**,
//! a pane **pill** — each nested under the level above it through
//! [`glyph_reveal`]'s dashed guide. The reveal, its cascade and the disclosure
//! chevron are Glyph's; this module contributes the rows, their tones, and which
//! of them the controller's signals say is open.
//!
//! **View-only.** Every gesture leaves through [`TreeActions`] rather than
//! reaching for the screen's own action bundle, so the tree composes identically
//! under a headless test and under the sheet that mounts it. [`TreeData`] is the
//! read side: a borrow of what the signals already hold.
//!
//! **Two backends, one tree.** herdr nests all three levels; zellij has no
//! spaces at all, so its tabs are *promoted* to the card shape and its panes
//! stay pills ([`TreeData::nested`] picks — and a herdr session reporting no
//! spaces yet takes the promoted shape too, rather than drawing nothing).
//!
//! **Accordion semantics belong to the controller** — at most one space and one
//! tab expanded. This module renders what the signals say and never flips them.
//!
//! **Two mounts, one tree** ([`TreeMode`]): the phone's modal sheet, and the
//! Tablet's docked rail, which adds the float/exited/bell marks and a per-row
//! ⋯ overflow button opening a Glyph menu. App widgets get no hover state, so
//! that button is always shown, in the dim ink, and takes the pane's cwd band.
//!
//! Two deliberate divergences from the design mock: its `:hover` accents have no
//! touch counterpart, so an add row keeps its dim dashed edge throughout (the
//! `status_row` precedent); and the reveal band's own top/bottom padding is not
//! expressible through [`RevealGuide`] — an inset child would join the cascade —
//! so only its leading indent is transcribed.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use frust::authoring::text::{FontStyle, TextOverflow};
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, DashPattern, EventCtx, EventResult,
    InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, Rect, SemanticsCtx, Size, View, Widget,
    build_child, rebuild_child, route_event_single, teardown_child,
};
use frust::{
    Align, Alignment, AnyView, BorderStyle, CrossAxisAlignment, EdgeInsets, Either, FlexChild,
    FlexView, GestureDetector, IconSource, Padding, SizedBox, Theme, any, column, container,
    flexible, icon, icons, inflexible, text,
};
use frust_glyph::motion::GlyphStagger;
use frust_glyph::{
    BadgeVariant, MenuEntry, RevealGuide, badge, glyph_chevron, glyph_reveal, menu_item,
};

use crate::features::terminal::domain::entities::{PaneEntry, Space, TabEntry};
use crate::ui::paint::{DASH_OFF, DASH_ON, HAIRLINE, RADIUS_FULL};
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;
use crate::ui::topbar::icon_button;

use super::util::{centered_row, h_gap, sized, stretch_column, v_gap};

/// `.space-row{border-radius:13px}` — the card level, worn by a space and by a
/// promoted zellij tab.
const CARD_RADIUS: f64 = 13.0;
/// `.tab-row{border-radius:7px}` — the mid level.
const TAB_RADIUS: f64 = 7.0;
/// `.add-row{border-radius:8px}` — a shade rounder than the row it follows.
const ADD_RADIUS: f64 = 8.0;

/// `.space-icon{width:20px;height:20px;font-size:12px}` — the mark's box and the
/// vector drawn inside it.
const SPACE_MARK_BOX: f64 = 20.0;
const SPACE_MARK_SIZE: f64 = 13.0;
/// `.tab-icon{width:15px;height:15px;border-radius:4px}`.
const TAB_BADGE_BOX: f64 = 15.0;
const TAB_BADGE_RADIUS: f64 = 4.0;
/// `.pane-icon{width:17px;height:17px}`.
const PANE_MARK_BOX: f64 = 17.0;
/// `.pane-focused-chip` — the mock's 8px `●`, drawn rather than typeset.
const FOCUS_DOT: f64 = 5.0;
/// `.pane-row .row-path{max-width:64px}` — a fixed band rather than a maximum:
/// nothing in `frust` caps a width without also tightening it, so a short cwd
/// reserves the same strip a long one ellipsizes into.
const PATH_WIDTH: f64 = 64.0;

const SPACE_NAME_SIZE: f32 = 11.5;
const TAB_NAME_SIZE: f32 = 10.5;
const TAB_BADGE_SIZE: f32 = 8.5;
const PANE_NAME_SIZE: f32 = 10.0;
const PANE_PATH_SIZE: f32 = 8.5;
const META_SIZE: f32 = 9.0;
const HINT_SIZE: f32 = 10.0;
const ADD_SIZE: f32 = 9.5;

/// Every row's inner gap — the mock's 9/8/8 across the three levels, which round
/// onto one token.
const ROW_GAP: f64 = spacing::SM;
/// `.space-group{margin-bottom:8px}` — between two top-level groups.
const GROUP_GAP: f64 = spacing::SM;
/// `.space-row{padding:12px 12px 12px 11px}` and `.add-space{padding:11px}`.
const CARD_PAD: EdgeInsets = EdgeInsets {
    left: spacing::BASE,
    top: spacing::BASE,
    right: spacing::BASE,
    bottom: spacing::BASE,
};
/// `.tab-row{padding:8px 10px}`.
const TAB_PAD: EdgeInsets = EdgeInsets {
    left: spacing::MD,
    top: spacing::SM,
    right: spacing::MD,
    bottom: spacing::SM,
};
/// `.pane-row{padding:7px 12px 7px 7px}` — the leading circle sits closer to the
/// edge than the trailing text does.
const PANE_PAD: EdgeInsets = EdgeInsets {
    left: spacing::XS,
    top: spacing::XS,
    right: spacing::BASE,
    bottom: spacing::XS,
};
/// `.add-row{padding:7px 10px}`.
const ADD_PAD: EdgeInsets = EdgeInsets {
    left: spacing::MD,
    top: spacing::XS,
    right: spacing::MD,
    bottom: spacing::XS,
};
/// `.empty-hint{padding:5px 2px}`.
const HINT_PAD: EdgeInsets = EdgeInsets {
    left: spacing::MICRO,
    top: spacing::TIGHT,
    right: spacing::MICRO,
    bottom: spacing::TIGHT,
};

/// `.tabs-inner{margin-left:20px; padding-left:14px; border-left:1px dashed}`.
const TABS_GUIDE: RevealGuide = RevealGuide::new(spacing::XXL, spacing::LG);
/// `.panes-inner` — the same rule one level in, in the accent ink: you drilled
/// into this tab.
const PANES_GUIDE: RevealGuide = RevealGuide {
    indent: spacing::XL,
    pad: spacing::BASE,
    accent: true,
};

/// The space row's leading mark — the mock's `◆` as a vector, since a font glyph
/// is never chrome (`crate::ui::topbar`'s rule). Drawn on the icon catalog's own
/// 24px design box so it scales like every other mark this app draws.
pub(crate) const SPACE_DIAMOND: IconSource = IconSource {
    d: "M 12 2 L 22 12 L 12 22 L 2 12 Z",
    design: 24.0,
};

/// The add rows' two runs — content text inside the sentence, not chrome, so the
/// `+` stays a run rather than becoming a vector.
const ADD_PLUS: &str = "+";
const ADD_SPACE_LABEL: &str = "new space";
const ADD_TAB_LABEL: &str = "new tab";
const ADD_PANE_LABEL: &str = "new pane";
/// `.empty-hint` — a space that has been read and has nothing in it.
const EMPTY_HINT: &str = "no tabs yet";

/// `.r-act{width:28px;height:28px}` and its 14px mark.
const OVERFLOW_BOX: f64 = 28.0;
const OVERFLOW_MARK: f64 = 14.0;
/// `.tab-row .bell .ic{width:13px}`.
const BELL_MARK: f64 = 13.0;
const BELL_LABEL: &str = "bell";
const FLOAT_MARK: &str = "float";
const EXITED_MARK: &str = "exited";

/// Where the tree is mounted — see the [module docs](self).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TreeMode {
    /// The phone's modal side sheet.
    Sheet,
    /// The Tablet's docked rail: marks and overflow menus on top of the sheet's rows.
    Docked,
}

/// The row an overflow menu was opened on, carrying what its entries act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowTarget {
    Space {
        id: String,
        name: String,
    },
    Tab {
        path: TabPath,
        tab_id: u64,
        label: String,
    },
    Pane {
        id: u32,
        is_plugin: bool,
        title: String,
    },
}

/// What an overflow menu entry asks for — [`row_menu_choice`] maps a picked index here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowMenuChoice {
    /// The row's own rename dialog.
    Rename,
    /// A tab under a space, or a pane under a tab.
    NewHere,
}

/// The overflow menu's one table for `target`: each entry beside the choice it
/// answers — rename everywhere, plus a create under a space or tab.
fn row_menu_table(target: &RowTarget) -> Vec<(Option<RowMenuChoice>, MenuEntry)> {
    let (rename, create) = match target {
        RowTarget::Space { .. } => ("Rename space", Some("New tab here")),
        RowTarget::Tab { .. } => ("Rename tab", Some("New pane here")),
        RowTarget::Pane { .. } => ("Rename pane", None),
    };
    let mut entries = vec![(
        Some(RowMenuChoice::Rename),
        menu_item(rename).icon(icons::EDIT),
    )];
    entries.extend(create.map(|label| {
        (
            Some(RowMenuChoice::NewHere),
            menu_item(label).icon(icons::ADD),
        )
    }));
    entries
}

/// The overflow menu for `target`, straight from [`row_menu_table`].
pub(crate) fn row_menu(target: &RowTarget) -> Vec<MenuEntry> {
    row_menu_table(target)
        .into_iter()
        .map(|(_, entry)| entry)
        .collect()
}

/// The [`RowMenuChoice`] behind entry `index` of [`row_menu`]`(target)`, read
/// back through the same table.
pub(crate) fn row_menu_choice(target: &RowTarget, index: usize) -> Option<RowMenuChoice> {
    row_menu_table(target)
        .get(index)
        .and_then(|(choice, _)| *choice)
}

/// Which tab a pane row belongs to — the identifiers a commit needs, resolved
/// once per row rather than re-derived inside a press handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabPath {
    /// The space holding the tab; `None` on a backend with no spaces.
    pub space_id: Option<String>,
    pub tab_key: String,
}

/// What the tree reads — a borrow of the controller's own signal values.
pub(crate) struct TreeData<'a> {
    /// Every space, in wire order. Empty on a backend with no spaces.
    pub spaces: &'a [Space],
    /// The **selected** space's tabs, and on zellij the whole tab world.
    pub tabs: &'a [TabEntry],
    /// A browsed non-selected space's tabs, keyed by the space they answer for.
    pub browsed: Option<&'a (String, Vec<TabEntry>)>,
    pub expanded_space: Option<&'a str>,
    pub expanded_tab: Option<&'a str>,
    /// Whether this backend has spaces — herdr's three levels vs zellij's two.
    pub has_spaces: bool,
}

impl TreeData<'_> {
    /// Whether the tree draws its three levels (space → tab → pane) or the
    /// promoted two.
    ///
    /// A backend **with** spaces that reports **none at all** takes the
    /// promoted shape too: nesting every tab under a space level that has no
    /// rows would hide the whole session behind an empty header. Its own
    /// create-space row survives that fallback (see [`tree_body`]) — it is the
    /// one way back to a space level.
    fn nested(&self) -> bool {
        self.has_spaces && !self.spaces.is_empty()
    }

    /// `space`'s tabs, or `None` when none have been read for it yet — a
    /// collapsed space nobody has browsed. `Some(&[])` is the different answer
    /// "read, and empty", which is what earns the hint and the tab count.
    fn tabs_of(&self, space: &Space) -> Option<&[TabEntry]> {
        if space.selected {
            return Some(self.tabs);
        }
        match self.browsed {
            Some((id, tabs)) if *id == space.id => Some(tabs.as_slice()),
            _ => None,
        }
    }
}

/// A bare press. Every callback here is generic in the mounting screen's own
/// `State`, since that is what a gesture runs over.
pub(crate) type Press<State> = Rc<dyn Fn(&mut State)>;
/// A disclosure row's tap: its id/key, and whether it is expanded **now** — so
/// the caller picks expand/collapse without re-reading a signal.
pub(crate) type PressToggle<State> = Rc<dyn Fn(&mut State, &str, bool)>;
/// A pane row's tap: its tab, its id, and which wire namespace it lives in.
pub(crate) type PressPane<State> = Rc<dyn Fn(&mut State, &TabPath, u32, bool)>;
/// A space rename: its id, and its current name (the dialog's seed).
pub(crate) type PressRenameSpace<State> = Rc<dyn Fn(&mut State, &str, &str)>;
/// A tab rename: its backend id, and its current label.
pub(crate) type PressRenameTab<State> = Rc<dyn Fn(&mut State, u64, &str)>;
/// A pane rename: its id, its wire namespace, and its current title.
pub(crate) type PressRenamePane<State> = Rc<dyn Fn(&mut State, u32, bool, &str)>;
/// A tab create, in a named space — `None` on a backend with no spaces.
pub(crate) type PressNewTab<State> = Rc<dyn Fn(&mut State, Option<&str>)>;
/// A pane create, in a named tab.
pub(crate) type PressNewPane<State> = Rc<dyn Fn(&mut State, &TabPath)>;
/// An overflow button's press: its window rect (the menu's anchor) and its row.
pub(crate) type PressMenu<State> = Rc<dyn Fn(&mut State, Rect, &RowTarget)>;

/// Every gesture the tree can fire. The three browse callbacks are always
/// wired; [`mutations`](Self::mutations) carries the rest.
pub(crate) struct TreeActions<State: 'static> {
    pub on_space: PressToggle<State>,
    pub on_tab: PressToggle<State>,
    /// The tree's one committing gesture.
    pub on_pane: PressPane<State>,
    /// Everything a read-only credential withdraws. `None` renders no add row
    /// and wires no press-and-hold at all, so the tree stays a pure browser.
    pub mutations: Option<TreeMutations<State>>,
}

/// The mutating half of [`TreeActions`] — the renames a press-and-hold opens and
/// the three create rows.
pub(crate) struct TreeMutations<State: 'static> {
    pub on_rename_space: PressRenameSpace<State>,
    pub on_rename_tab: PressRenameTab<State>,
    pub on_rename_pane: PressRenamePane<State>,
    pub on_new_space: Press<State>,
    pub on_new_tab: PressNewTab<State>,
    pub on_new_pane: PressNewPane<State>,
    /// The docked rail's overflow menus; `None` draws no overflow button.
    pub on_menu: Option<PressMenu<State>>,
}

/// The theme and palette, resolved once for a whole tree build rather than per
/// row.
struct Tones<'a> {
    theme: &'a Theme,
    colors: Palette,
    mode: TreeMode,
}

/// One level's box — the four things the three row shapes differ in.
struct RowBox {
    fill: Color,
    border: Color,
    radius: f64,
    pad: EdgeInsets,
}

/// A disclosure row's text: its name, and the trailing count when one is known.
struct RowLabel<'a> {
    name: &'a str,
    size: f32,
    ink: Color,
    meta: Option<String>,
}

/// The side sheet's tree body, top to bottom: a group per space (or, with no
/// spaces, per tab) and the create row that closes the level.
pub(crate) fn tree_body<State: 'static>(
    data: &TreeData<'_>,
    actions: &TreeActions<State>,
    mode: TreeMode,
) -> impl View<State> + use<State> {
    let theme = active_theme();
    let tones = Tones {
        colors: Palette::from_theme(&theme),
        theme: &theme,
        mode,
    };
    let mut children: Vec<AnyView<State>> = Vec::new();

    if data.nested() {
        for space in data.spaces {
            children.push(any(space_group(space, data, &tones, actions))); // erasure: keep accumulator into Vec<AnyView>
            children.push(any(v_gap(GROUP_GAP))); // erasure: keep accumulator into Vec<AnyView>
        }
    } else {
        for tab in data.tabs {
            let expanded = data.expanded_tab == Some(tab.key.as_str());
            children.push(any(tab_group(tab, None, true, expanded, &tones, actions))); // erasure: keep accumulator into Vec<AnyView>
            children.push(any(v_gap(GROUP_GAP))); // erasure: keep accumulator into Vec<AnyView>
        }
        if let Some(mutations) = &actions.mutations {
            let create = Rc::clone(&mutations.on_new_tab);
            children.push(any(add_row(
                ADD_TAB_LABEL,
                CARD_RADIUS,
                CARD_PAD,
                &tones,
                move |state: &mut State| create(state, None),
            )));
        }
    }
    // The space level's own create row closes the tree on either shape: nested,
    // it follows the last space; promoted-because-empty, it is what puts the
    // first space back.
    if let Some(mutations) = actions.mutations.as_ref().filter(|_| data.has_spaces) {
        let create = Rc::clone(&mutations.on_new_space);
        children.push(any(add_row(
            ADD_SPACE_LABEL,
            CARD_RADIUS,
            CARD_PAD,
            &tones,
            move |state: &mut State| create(state),
        )));
    }

    stretch_column(children)
}

/// The auto-expansion a landed space cascade schedules: which tab opens, and how
/// long after the space's own tap.
///
/// **The numbers live here, the timer lives in the wiring**: the delay is a
/// property of the reveal this module builds (its height tween and its child
/// cascade run in parallel, so the whole thing has settled at the longer of the
/// two), while spawning belongs with the callback that owns the controller.
/// `can_create` counts the trailing add row, which rides the same cascade.
///
/// The target is the space's active tab, else its first; a space with no tabs
/// auto-expands nothing.
pub(crate) fn auto_expand_after_cascade(
    tabs: &[TabEntry],
    can_create: bool,
) -> Option<(String, Duration)> {
    let target = tabs
        .iter()
        .find(|tab| tab.active)
        .or_else(|| tabs.first())?;
    let theme = active_theme();
    let rows = tabs.len() + usize::from(can_create);
    let delay = cascade_settle(
        rows,
        theme.motion.durations.base,
        theme.motion.reduce_motion,
    );
    Some((target.key.clone(), delay))
}

/// How long a reveal of `rows` children takes to settle. `reduce_motion` snaps
/// both clocks, so there is nothing left to wait for.
fn cascade_settle(rows: usize, base_ms: f64, reduce_motion: bool) -> Duration {
    if reduce_motion {
        return Duration::ZERO;
    }
    let cascade_ms = GlyphStagger::glyph().total_duration(rows);
    Duration::from_secs_f64(base_ms.max(cascade_ms).max(0.0) / 1000.0)
}

/// One space: its card row, and the reveal holding its tabs.
fn space_group<State: 'static>(
    space: &Space,
    data: &TreeData<'_>,
    tones: &Tones<'_>,
    actions: &TreeActions<State>,
) -> impl View<State> {
    let expanded = data.expanded_space == Some(space.id.as_str());
    let tabs = data.tabs_of(space);
    let head = space_row(space, tabs.map(<[TabEntry]>::len), expanded, tones, actions);

    let mut kids: Vec<AnyView<State>> = Vec::new();
    match tabs {
        // Read, and empty: the hint. Unread (a fetch still in flight) shows
        // nothing rather than claiming emptiness.
        Some([]) => kids.push(any(empty_hint(tones))), // erasure: keep accumulator into Vec<AnyView>
        Some(tabs) => {
            for tab in tabs {
                let tab_expanded = expanded && data.expanded_tab == Some(tab.key.as_str());
                kids.push(any(tab_group(
                    tab,
                    Some(space.id.as_str()),
                    false,
                    tab_expanded,
                    tones,
                    actions,
                )));
            }
        }
        None => {}
    }
    if let Some(mutations) = &actions.mutations {
        let create = Rc::clone(&mutations.on_new_tab);
        let id = space.id.clone();
        kids.push(any(add_row(
            ADD_TAB_LABEL,
            ADD_RADIUS,
            ADD_PAD,
            tones,
            move |state: &mut State| create(state, Some(&id)),
        )));
    }

    column()
        .child(head)
        .child(
            glyph_reveal(kids)
                .expanded(expanded)
                .gap(spacing::XS)
                .guide(TABS_GUIDE),
        )
        .cross_axis(CrossAxisAlignment::Stretch)
}

/// One tab: its row, and the reveal holding its panes. `promoted` gives a
/// zellij tab the card shape a space would have worn.
fn tab_group<State: 'static>(
    tab: &TabEntry,
    space_id: Option<&str>,
    promoted: bool,
    expanded: bool,
    tones: &Tones<'_>,
    actions: &TreeActions<State>,
) -> impl View<State> {
    let path = TabPath {
        space_id: space_id.map(str::to_string),
        tab_key: tab.key.clone(),
    };
    let head = tab_row(tab, &path, promoted, expanded, tones, actions);

    let mut kids: Vec<AnyView<State>> = tab
        .panes
        .iter()
        .map(|pane| any(pane_row(pane, &path, tones, actions))) // erasure: keep accumulator into Vec<AnyView>
        .collect();
    if let Some(mutations) = &actions.mutations {
        let create = Rc::clone(&mutations.on_new_pane);
        let path = path.clone();
        kids.push(any(add_row(
            ADD_PANE_LABEL,
            RADIUS_FULL,
            ADD_PAD,
            tones,
            move |state: &mut State| create(state, &path),
        )));
    }

    column()
        .child(head)
        .child(
            glyph_reveal(kids)
                .expanded(expanded)
                .gap(spacing::XS)
                .guide(PANES_GUIDE),
        )
        .cross_axis(CrossAxisAlignment::Stretch)
}

/// `.space-row` — the accent mark, the name, the tab count when it is known, and
/// the chevron. A tap toggles the space; a hold renames it.
fn space_row<State: 'static>(
    space: &Space,
    tab_count: Option<usize>,
    expanded: bool,
    tones: &Tones<'_>,
    actions: &TreeActions<State>,
) -> impl View<State> {
    let mark = SizedBox::<State>(Some(SPACE_MARK_BOX), Some(SPACE_MARK_BOX)).child(Align(
        Alignment::CENTER,
        icon(SPACE_DIAMOND)
            .size(SPACE_MARK_SIZE)
            .color(tones.colors.amber),
    ));
    let content = disclosure_row(
        mark,
        RowLabel {
            name: &space.name,
            size: SPACE_NAME_SIZE,
            ink: tones.colors.fg,
            meta: tab_count.map(|count| count_label(count, "tab")),
        },
        None::<AnyView<State>>,
        expanded,
        tones,
    );

    let toggle = Rc::clone(&actions.on_space);
    let id = space.id.clone();
    let hold = actions.mutations.as_ref().map(|mutations| {
        let rename = Rc::clone(&mutations.on_rename_space);
        let (id, name) = (space.id.clone(), space.name.clone());
        Rc::new(move |state: &mut State| rename(state, &id, &name)) as Press<State>
    });
    let target = RowTarget::Space {
        id: space.id.clone(),
        name: space.name.clone(),
    };
    boxed_row(
        content,
        card_box(expanded, false, tones),
        move |state: &mut State| toggle(state, &id, expanded),
        hold,
        overflow(target, actions, tones),
    )
}

/// `.tab-row` — the 1-based index badge, the label and the chevron. A tap
/// browses (never commits); a hold renames.
fn tab_row<State: 'static>(
    tab: &TabEntry,
    path: &TabPath,
    promoted: bool,
    expanded: bool,
    tones: &Tones<'_>,
    actions: &TreeActions<State>,
) -> impl View<State> {
    // The badge's ink is the one role read straight off the scheme: `Palette`
    // carries no on-accent colour, and an amber fill's ink is that role's own
    // pair (`#241a04` under Glyph dark, the mock's own value).
    let (badge_fill, badge_ink) = if tab.active {
        (tones.colors.amber, tones.theme.scheme().on_primary)
    } else {
        (tones.colors.raised, tones.colors.fg_dim)
    };
    let index_badge = container(
        SizedBox::<State>(Some(TAB_BADGE_BOX), Some(TAB_BADGE_BOX)).child(Align(
            Alignment::CENTER,
            text(tab.index.to_string()).style(sized(
                &tones.theme.type_scale.label_small,
                TAB_BADGE_SIZE,
                badge_ink,
            )),
        )),
    )
    .fill(badge_fill)
    .radius(TAB_BADGE_RADIUS);

    let boxed = if promoted {
        card_box(expanded, tab.active, tones)
    } else {
        RowBox {
            fill: if tab.active {
                tones.colors.amber_faint
            } else {
                tones.colors.surface
            },
            border: if tab.active {
                tones.colors.amber_border
            } else {
                tones.colors.outline
            },
            radius: TAB_RADIUS,
            pad: TAB_PAD,
        }
    };
    let bell = (tones.mode == TreeMode::Docked && tab.has_bell).then(|| {
        icon(icons::NOTIFICATIONS)
            .size(BELL_MARK)
            .color(tones.colors.warning)
            .label(BELL_LABEL)
    });
    let content = disclosure_row(
        index_badge,
        RowLabel {
            name: &tab.label,
            size: if promoted {
                SPACE_NAME_SIZE
            } else {
                TAB_NAME_SIZE
            },
            ink: tones.colors.fg,
            // A promoted tab stands where a space's card does, so it carries the
            // same trailing count — of what it actually holds.
            meta: promoted.then(|| count_label(tab.panes.len(), "pane")),
        },
        bell,
        expanded,
        tones,
    );

    let toggle = Rc::clone(&actions.on_tab);
    let key = tab.key.clone();
    let hold = actions.mutations.as_ref().map(|mutations| {
        let rename = Rc::clone(&mutations.on_rename_tab);
        let (id, label) = (tab.tab_id, tab.label.clone());
        Rc::new(move |state: &mut State| rename(state, id, &label)) as Press<State>
    });
    let target = RowTarget::Tab {
        path: path.clone(),
        tab_id: tab.tab_id,
        label: tab.label.clone(),
    };
    boxed_row(
        content,
        boxed,
        move |state: &mut State| toggle(state, &key, expanded),
        hold,
        overflow(target, actions, tones),
    )
}

/// `.pane-row` — the leaf pill: a circle mark, the title, the cwd, and the
/// accent dot a focused pane wears. A tap commits; a hold renames.
///
/// **The badge is the tab's own focus**, as the last read recorded it — unlike
/// the flat sheet's rows, which only badge the tab the session is on. A tree
/// draws a whole containment world at once, so each tab reads its own.
fn pane_row<State: 'static>(
    pane: &PaneEntry,
    path: &TabPath,
    tones: &Tones<'_>,
    actions: &TreeActions<State>,
) -> impl View<State> {
    let colors = &tones.colors;
    let (fill, border, mark) = if pane.focused {
        (colors.amber_faint, colors.amber_border, colors.amber)
    } else {
        (colors.base, colors.outline, colors.raised)
    };
    let title_ink = if pane.exited {
        colors.fg_dim
    } else {
        colors.fg
    };

    let mut children: Vec<FlexChild<State>> = vec![
        inflexible(
            container(SizedBox::<State>(Some(PANE_MARK_BOX), Some(PANE_MARK_BOX)))
                .fill(mark)
                .radius(RADIUS_FULL),
        ),
        h_gap(ROW_GAP),
        flexible(
            1,
            text(pane.title.clone())
                .style(sized(
                    &tones.theme.type_scale.body_small,
                    PANE_NAME_SIZE,
                    title_ink,
                ))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
    ];
    let docked = tones.mode == TreeMode::Docked;
    if docked && pane.floating {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(badge(FLOAT_MARK, BadgeVariant::Info)));
    }
    if docked && pane.exited {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(badge(EXITED_MARK, BadgeVariant::Neutral)));
    }
    // A backend that reports no cwd leaves no gutter reserved for one; the rail
    // gives the band to the overflow button (`.pane-row:hover .r-path`).
    if !pane.cwd.is_empty() && !docked {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(
            SizedBox::<State>(Some(PATH_WIDTH), None).child(
                text(pane.cwd.clone())
                    .style(sized(
                        &tones.theme.type_scale.label_small,
                        PANE_PATH_SIZE,
                        colors.fg_dim,
                    ))
                    .max_lines(1)
                    .overflow(TextOverflow::Ellipsis),
            ),
        ));
    }
    if pane.focused {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(
            container(SizedBox::<State>(Some(FOCUS_DOT), Some(FOCUS_DOT)))
                .fill(colors.amber)
                .radius(RADIUS_FULL),
        ));
    }

    let boxed = RowBox {
        fill,
        border,
        radius: RADIUS_FULL,
        pad: PANE_PAD,
    };

    let commit = Rc::clone(&actions.on_pane);
    let (target, id, is_plugin) = (path.clone(), pane.id, pane.is_plugin);
    let hold = actions.mutations.as_ref().map(|mutations| {
        let rename = Rc::clone(&mutations.on_rename_pane);
        let title = pane.title.clone();
        Rc::new(move |state: &mut State| rename(state, id, is_plugin, &title)) as Press<State>
    });
    let menu_target = RowTarget::Pane {
        id,
        is_plugin,
        title: pane.title.clone(),
    };
    boxed_row(
        centered_row(children),
        boxed,
        move |state: &mut State| commit(state, &target, id, is_plugin),
        hold,
        overflow(menu_target, actions, tones),
    )
}

/// The row content every disclosure level shares: leading mark, ellipsized name,
/// an optional mark after it, the optional trailing count, chevron.
fn disclosure_row<State: 'static>(
    leading: impl View<State>,
    label: RowLabel<'_>,
    mark: Option<impl View<State>>,
    expanded: bool,
    tones: &Tones<'_>,
) -> FlexView<State> {
    let mut children: Vec<FlexChild<State>> = vec![
        inflexible(leading),
        h_gap(ROW_GAP),
        flexible(
            1,
            text(label.name.to_string())
                .style(sized(
                    &tones.theme.type_scale.body_small,
                    label.size,
                    label.ink,
                ))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
    ];
    if let Some(mark) = mark {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(mark));
    }
    if let Some(meta) = label.meta {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(text(meta).style(sized(
            &tones.theme.type_scale.label_small,
            META_SIZE,
            tones.colors.fg_dim,
        ))));
    }
    children.push(h_gap(ROW_GAP));
    children.push(inflexible(glyph_chevron(expanded)));
    centered_row(children)
}

/// A row's `content` in its level's box, with its gestures: the whole box presses
/// on the sheet; with an overflow button, the content alone does, so the button's
/// own press never also taps the row it sits in.
fn boxed_row<State: 'static>(
    content: FlexView<State>,
    boxed: RowBox,
    on_tap: impl Fn(&mut State) + 'static,
    on_hold: Option<Press<State>>,
    overflow: Option<impl View<State>>,
) -> impl View<State> {
    let frame = |child: FlexView<State>| {
        container(Padding(boxed.pad, child))
            .fill(boxed.fill)
            .radius(boxed.radius)
            .border(boxed.border, HAIRLINE)
    };
    match overflow {
        None => Either::Left(pressable(frame(content), on_tap, on_hold)),
        Some(button) => Either::Right(frame(centered_row(vec![
            flexible(1, pressable(content, on_tap, on_hold)),
            h_gap(spacing::TIGHT),
            inflexible(button),
        ]))),
    }
}

/// The rail's ⋯ button for `target`, or `None` on the sheet and for a read-only
/// credential (no [`TreeMutations::on_menu`]).
fn overflow<State: 'static>(
    target: RowTarget,
    actions: &TreeActions<State>,
    tones: &Tones<'_>,
) -> Option<impl View<State>> {
    if tones.mode != TreeMode::Docked {
        return None;
    }
    let open = Rc::clone(actions.mutations.as_ref()?.on_menu.as_ref()?);
    let anchor = Rc::new(Cell::new(Rect::ZERO));
    let at = Rc::clone(&anchor);
    let label = match &target {
        RowTarget::Space { name, .. } => format!("{name} actions"),
        RowTarget::Tab { label, .. } => format!("{label} actions"),
        RowTarget::Pane { title, .. } => format!("{title} actions"),
    };
    let button = icon_button(icons::MORE_HORIZ, label, move |state: &mut State| {
        open(state, at.get(), &target)
    })
    .ink(tones.colors.fg_dim)
    .icon_size(OVERFLOW_MARK)
    .box_size(Size::new(OVERFLOW_BOX, OVERFLOW_BOX))
    .radius(tones.theme.shape.small);
    Some(AnchorProbe {
        child: any(button), // erasure: keep ChildPod-fed
        anchor,
    })
}

/// Records its child's painted **window** rect into `anchor` — a Glyph menu is
/// anchored in window space, and only paint learns where a widget landed.
struct AnchorProbe<State: 'static> {
    child: AnyView<State>,
    anchor: Rc<Cell<Rect>>,
}

/// The retained half of [`AnchorProbe`].
struct AnchorProbeWidget {
    child: ChildPod,
    anchor: Rc<Cell<Rect>>,
}

impl<State: 'static> View<State> for AnchorProbe<State> {
    type Element = AnchorProbeWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> AnchorProbeWidget {
        AnchorProbeWidget {
            child: build_child(&self.child, ctx),
            anchor: Rc::clone(&self.anchor),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut AnchorProbeWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // A press can land before the next paint: carry the last rect over.
        self.anchor.set(element.anchor.get());
        element.anchor = Rc::clone(&self.anchor);
        rebuild_child(&prev.child, &self.child, &mut element.child, ctx)
    }

    fn teardown(&self, element: &mut AnchorProbeWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for AnchorProbeWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let rect = Rect::from_origin_size(ctx.origin(), self.child.size());
        self.anchor.set(rect);
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

/// The card level's box. Expansion raises the fill and accents the edge
/// (`.space-group.expanded > .space-row`); `active` is the promoted tab's own
/// selection wash on top of it.
fn card_box(expanded: bool, active: bool, tones: &Tones<'_>) -> RowBox {
    let colors = &tones.colors;
    RowBox {
        fill: if active {
            colors.amber_faint
        } else if expanded {
            colors.raised
        } else {
            colors.base
        },
        border: if expanded || active {
            colors.amber_border
        } else {
            colors.outline
        },
        radius: CARD_RADIUS,
        pad: CARD_PAD,
    }
}

/// `.add-row` — a dashed create row at its level's own radius. The `+` is the
/// accent because it is the call to action.
fn add_row<State: 'static, F: Fn(&mut State) + 'static>(
    label: &'static str,
    radius: f64,
    pad: EdgeInsets,
    tones: &Tones<'_>,
    on_tap: F,
) -> impl View<State> {
    let plus = sized(
        &tones.theme.type_scale.body_small,
        ADD_SIZE,
        tones.colors.amber,
    );
    let body = sized(
        &tones.theme.type_scale.body_small,
        ADD_SIZE,
        tones.colors.fg_dim,
    );
    let content = centered_row(vec![
        inflexible(text(ADD_PLUS).style(plus)),
        h_gap(spacing::TIGHT),
        inflexible(text(label).style(body)),
    ]);
    GestureDetector(
        container(Padding(pad, Align(Alignment::CENTER, content)))
            .fill(Color::TRANSPARENT)
            .radius(radius)
            .border(tones.colors.outline, HAIRLINE)
            .border_style(BorderStyle::Dashed(DashPattern::new(DASH_ON, DASH_OFF))),
    )
    .on_tap(on_tap)
}

/// `.empty-hint` — italic, dim, and not a row: a space that has been read and
/// holds nothing.
fn empty_hint<State: 'static>(tones: &Tones<'_>) -> impl View<State> {
    let mut style = sized(
        &tones.theme.type_scale.body_small,
        HINT_SIZE,
        tones.colors.fg_dim,
    );
    style.style = FontStyle::Italic;
    Padding(HINT_PAD, text(EMPTY_HINT).style(style))
}

/// The row gesture pair: a tap, plus a press-and-hold when the caller supplied
/// one. [`GestureDetector`] resolves exactly one per press, and a slop-drag
/// cancels both.
fn pressable<State: 'static, V: View<State> + 'static>(
    child: V,
    on_tap: impl Fn(&mut State) + 'static,
    on_hold: Option<Press<State>>,
) -> impl View<State> {
    let detector = GestureDetector(child).on_tap(on_tap);
    match on_hold {
        Some(hold) => Either::Left(detector.on_long_press(move |state: &mut State| hold(state))),
        None => Either::Right(detector),
    }
}

/// `4 tabs`/`1 tab` — the mock's own pluralisation.
fn count_label(count: usize, singular: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {singular}{plural}")
}

#[cfg(test)]
mod tests {
    use super::*;

    use frust_core::{
        FrameTime, InputEvent, PaintScene, PointerButton, PointerEvent, PointerPhase, RenderRoot,
    };
    use frust_text::TextContext;
    use kurbo::{Point, Size};

    /// Comfortably past `GestureDetector`'s own 500ms long-press threshold.
    const HELD_MS: f64 = 700.0;
    const SURFACE: Size = Size::new(320.0, 900.0);

    /// A GPU-free paint target recording every container's `(box, radius, fill)`
    /// — enough to tell the three row shapes apart by their radii.
    #[derive(Default)]
    struct Rec {
        rounded: Vec<(Point, Size, f64, Color)>,
    }

    impl PaintScene for Rec {
        fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
        fn draw_text(&mut self, _origin: Point, _text: &str) {}
        fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, color: Color) {
            self.rounded.push((origin, size, radius, color));
        }
    }

    impl Rec {
        /// Every *filled* box painted at `radius`, in paint order — the rows
        /// themselves. A dashed create row fills transparently, so this is what
        /// separates the two at a shared radius.
        fn filled_at(&self, radius: f64) -> Vec<(Point, Size)> {
            self.boxes_at(radius, false)
        }

        /// Every transparent box painted at `radius` — the create rows.
        fn dashed_at(&self, radius: f64) -> Vec<(Point, Size)> {
            self.boxes_at(radius, true)
        }

        fn boxes_at(&self, radius: f64, transparent: bool) -> Vec<(Point, Size)> {
            self.rounded
                .iter()
                .filter(|(_, _, r, fill)| {
                    *r == radius && (*fill == Color::TRANSPARENT) == transparent
                })
                .map(|(origin, size, _, _)| (*origin, *size))
                .collect()
        }
    }

    /// What the tree's callbacks recorded — the `State` every test mounts over.
    #[derive(Default)]
    struct Log {
        spaces: Vec<(String, bool)>,
        tabs: Vec<(String, bool)>,
        panes: Vec<(Option<String>, String, u32)>,
        renames: Vec<String>,
        creates: Vec<String>,
    }

    fn pane(id: u32, title: &str, focused: bool) -> PaneEntry {
        PaneEntry {
            id,
            title: title.to_string(),
            cwd: "/home/ed".to_string(),
            focused,
            floating: false,
            exited: false,
            is_plugin: false,
            is_fullscreen: false,
            x: 0,
            y: 0,
            rows: 24,
            cols: 80,
        }
    }

    fn tab(index: u32, key: &str, active: bool, panes: Vec<PaneEntry>) -> TabEntry {
        TabEntry {
            index,
            key: key.to_string(),
            tab_id: u64::from(index),
            label: key.to_string(),
            pane_title: String::new(),
            pane_name: String::new(),
            pane_path: String::new(),
            panes,
            active,
            has_bell: false,
        }
    }

    fn space(id: &str, selected: bool) -> Space {
        Space {
            id: id.to_string(),
            name: id.to_string(),
            selected,
        }
    }

    /// Two tabs under the selected space; the second is active and holds two
    /// panes, the second of them focused.
    fn tabs() -> Vec<TabEntry> {
        vec![
            tab(1, "alpha", false, vec![pane(1, "zsh", false)]),
            tab(
                2,
                "devbox",
                true,
                vec![pane(2, "zsh", false), pane(3, "htop", true)],
            ),
        ]
    }

    fn actions(mutating: bool) -> TreeActions<Log> {
        TreeActions {
            on_space: Rc::new(|log: &mut Log, id: &str, expanded: bool| {
                log.spaces.push((id.to_string(), expanded));
            }),
            on_tab: Rc::new(|log: &mut Log, key: &str, expanded: bool| {
                log.tabs.push((key.to_string(), expanded));
            }),
            on_pane: Rc::new(|log: &mut Log, path: &TabPath, id: u32, _plugin: bool| {
                log.panes
                    .push((path.space_id.clone(), path.tab_key.clone(), id));
            }),
            mutations: mutating.then(|| TreeMutations {
                on_rename_space: Rc::new(|log: &mut Log, _id: &str, name: &str| {
                    log.renames.push(format!("space:{name}"));
                }),
                on_rename_tab: Rc::new(|log: &mut Log, _id: u64, name: &str| {
                    log.renames.push(format!("tab:{name}"));
                }),
                on_rename_pane: Rc::new(|log: &mut Log, _id: u32, _plugin: bool, title: &str| {
                    log.renames.push(format!("pane:{title}"));
                }),
                on_new_space: Rc::new(|log: &mut Log| log.creates.push("space".to_string())),
                on_new_tab: Rc::new(|log: &mut Log, space: Option<&str>| {
                    log.creates
                        .push(format!("tab:{}", space.unwrap_or("(none)")));
                }),
                on_new_pane: Rc::new(|log: &mut Log, path: &TabPath| {
                    log.creates.push(format!("pane:{}", path.tab_key));
                }),
                on_menu: None,
            }),
        }
    }

    /// A mounted tree: the render root, its recorded callbacks, and the text
    /// context its rows need to measure.
    struct Rig<V: View<Log>> {
        root: RenderRoot<Log, V>,
        state: Log,
        tcx: TextContext,
    }

    impl<V: View<Log>> Rig<V> {
        fn new() -> Self {
            Self {
                root: RenderRoot::new(),
                state: Log::default(),
                tcx: TextContext::new(),
            }
        }

        /// One rebuild + layout + paint at `ms`, returning what was painted.
        fn frame(&mut self, logic: &mut impl FnMut(&mut Log) -> V, ms: f64) -> Rec {
            self.root.rebuild(logic, &mut self.state);
            let tcx: &mut dyn std::any::Any = &mut self.tcx;
            self.root.layout_with_text(SURFACE, tcx);
            let mut rec = Rec::default();
            self.root
                .paint(&mut rec, FrameTime::from_nanos((ms * 1_000_000.0) as u64));
            rec
        }

        fn dispatch(&mut self, phase: PointerPhase, at: Point) {
            self.root.event(
                &mut self.state,
                &InputEvent::Pointer(PointerEvent {
                    phase,
                    position: at,
                    button: PointerButton::Primary,
                }),
            );
        }

        fn tap(&mut self, at: Point) {
            self.dispatch(PointerPhase::Down, at);
            self.dispatch(PointerPhase::Up, at);
        }

        /// A press held past the recognizer's threshold: two paints span it, and
        /// the release resolves as a long press rather than a tap.
        fn hold(&mut self, logic: &mut impl FnMut(&mut Log) -> V, at: Point) {
            self.dispatch(PointerPhase::Down, at);
            self.frame(logic, 0.0);
            self.frame(logic, HELD_MS);
            self.dispatch(PointerPhase::Up, at);
        }
    }

    /// The center of `(origin, size)`.
    fn center((origin, size): (Point, Size)) -> Point {
        Point::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0)
    }

    #[test]
    fn the_three_levels_each_paint_their_own_shape() {
        let (spaces, tabs) = (vec![space("ws-1", true)], tabs());
        let acts = actions(true);
        let mut logic = |_state: &mut Log| {
            tree_body(
                &TreeData {
                    spaces: &spaces,
                    tabs: &tabs,
                    browsed: None,
                    expanded_space: Some("ws-1"),
                    expanded_tab: Some("devbox"),
                    has_spaces: true,
                },
                &acts,
                TreeMode::Sheet,
            )
        };
        let mut rig = Rig::new();
        let rec = rig.frame(&mut logic, 0.0);

        assert_eq!(rec.filled_at(CARD_RADIUS).len(), 1, "one space card");
        assert_eq!(rec.filled_at(TAB_RADIUS).len(), 2, "both tab rects");
        // Two pane pills under the expanded tab, plus the pill-shaped mark each
        // carries and the focused row's dot.
        assert!(
            rec.filled_at(RADIUS_FULL).len() >= 5,
            "the pane level is pills: {}",
            rec.filled_at(RADIUS_FULL).len()
        );
        assert_eq!(
            rec.filled_at(TAB_BADGE_RADIUS).len(),
            2,
            "one index badge per tab row"
        );
        // One create row per level, each at its own level's radius.
        assert_eq!(rec.dashed_at(CARD_RADIUS).len(), 1, "+ new space");
        assert_eq!(rec.dashed_at(ADD_RADIUS).len(), 1, "+ new tab");
        assert_eq!(rec.dashed_at(RADIUS_FULL).len(), 1, "+ new pane");
    }

    #[test]
    fn a_resting_expanded_tree_paints_every_row_in_its_first_frame() {
        // The reveal's settled-on-first-build contract: an already-open tree
        // draws in one frame rather than cascading in.
        let (spaces, tabs) = (vec![space("ws-1", true)], tabs());
        let acts = actions(false);
        let mut logic = |_state: &mut Log| {
            tree_body(
                &TreeData {
                    spaces: &spaces,
                    tabs: &tabs,
                    browsed: None,
                    expanded_space: Some("ws-1"),
                    expanded_tab: Some("devbox"),
                    has_spaces: true,
                },
                &acts,
                TreeMode::Sheet,
            )
        };
        let mut rig = Rig::new();
        let rec = rig.frame(&mut logic, 0.0);
        assert_eq!(rec.filled_at(TAB_RADIUS).len(), 2, "both tabs, frame one");

        // The cascade would still be mid-flight at 90ms, so a tree that painted
        // everything now was settled, not animating.
        let later = rig.frame(&mut logic, 90.0);
        assert_eq!(later.filled_at(TAB_RADIUS).len(), 2);
    }

    #[test]
    fn a_post_mount_expansion_cascades_its_children_in() {
        // The other half of the settled-on-first-build contract: a flip *after*
        // the mount animates, which also pins that the reveal widget is retained
        // across rebuilds rather than rebuilt settled every frame.
        let (spaces, tabs) = (vec![space("ws-1", true)], tabs());
        let acts = actions(false);
        let open = std::cell::Cell::new(false);
        let mut logic = |_state: &mut Log| {
            tree_body(
                &TreeData {
                    spaces: &spaces,
                    tabs: &tabs,
                    browsed: None,
                    expanded_space: open.get().then_some("ws-1"),
                    expanded_tab: None,
                    has_spaces: true,
                },
                &acts,
                TreeMode::Sheet,
            )
        };
        let mut rig = Rig::new();
        let closed = rig.frame(&mut logic, 0.0);
        assert!(
            closed.filled_at(TAB_RADIUS).is_empty(),
            "a collapsed space paints nothing under its card"
        );

        open.set(true);
        rig.frame(&mut logic, 0.0);
        // 60ms in, the second child's own 90ms window has not opened yet.
        let mid = rig.frame(&mut logic, 60.0);
        assert_eq!(mid.filled_at(TAB_RADIUS).len(), 1, "the cascade trails");
        let settled = rig.frame(&mut logic, 600.0);
        assert_eq!(settled.filled_at(TAB_RADIUS).len(), 2, "…then both are in");
    }

    #[test]
    fn zellij_promotes_its_tabs_to_the_card_shape_and_draws_no_space_row() {
        let (spaces, tabs) = (Vec::new(), tabs());
        let acts = actions(true);
        let mut logic = |_state: &mut Log| {
            tree_body(
                &TreeData {
                    spaces: &spaces,
                    tabs: &tabs,
                    browsed: None,
                    expanded_space: None,
                    expanded_tab: Some("devbox"),
                    has_spaces: false,
                },
                &acts,
                TreeMode::Sheet,
            )
        };
        let mut rig = Rig::new();
        let rec = rig.frame(&mut logic, 0.0);

        assert_eq!(rec.filled_at(CARD_RADIUS).len(), 2, "tabs wear the card");
        assert!(
            rec.filled_at(TAB_RADIUS).is_empty(),
            "…and nothing wears the nested rect"
        );
        assert!(
            rec.filled_at(RADIUS_FULL).len() >= 2,
            "panes stay pills under a promoted tab"
        );
        // One top-level create row, and no space level for one to sit under.
        assert_eq!(rec.dashed_at(CARD_RADIUS).len(), 1, "+ new tab");
        assert!(rec.dashed_at(ADD_RADIUS).is_empty());
    }

    #[test]
    fn a_backend_with_spaces_but_none_yet_promotes_its_tabs_too() {
        // A herdr session whose space list came back empty: nesting every tab
        // under a level with no rows would draw a session that looks empty.
        let (spaces, tabs) = (Vec::new(), tabs());
        let acts = actions(true);
        let mut logic = |_state: &mut Log| {
            tree_body(
                &TreeData {
                    spaces: &spaces,
                    tabs: &tabs,
                    browsed: None,
                    expanded_space: None,
                    expanded_tab: Some("devbox"),
                    has_spaces: true,
                },
                &acts,
                TreeMode::Sheet,
            )
        };
        let mut rig = Rig::new();
        let rec = rig.frame(&mut logic, 0.0);

        assert_eq!(
            rec.filled_at(CARD_RADIUS).len(),
            2,
            "both tabs are on screen, wearing the card"
        );
        assert!(rec.filled_at(TAB_RADIUS).is_empty(), "…and none is nested");
        assert_eq!(
            rec.dashed_at(CARD_RADIUS).len(),
            2,
            "+ new tab, and the + new space row that puts the level back"
        );

        // …and the create rows still report the level they were drawn under.
        rig.tap(center(rec.dashed_at(CARD_RADIUS)[0]));
        assert_eq!(rig.state.creates, vec!["tab:(none)".to_string()]);
        rig.tap(center(rec.dashed_at(CARD_RADIUS)[1]));
        assert_eq!(
            rig.state.creates,
            vec!["tab:(none)".to_string(), "space".to_string()]
        );
    }

    #[test]
    fn a_space_row_counts_its_tabs_only_once_they_are_known() {
        let spaces = vec![space("ws-1", true), space("ws-2", false)];
        let tabs = tabs();
        let browsed = ("ws-2".to_string(), vec![tab(1, "beta", true, Vec::new())]);
        let data = TreeData {
            spaces: &spaces,
            tabs: &tabs,
            browsed: Some(&browsed),
            expanded_space: Some("ws-2"),
            expanded_tab: None,
            has_spaces: true,
        };
        assert_eq!(data.tabs_of(&spaces[0]).map(<[TabEntry]>::len), Some(2));
        assert_eq!(data.tabs_of(&spaces[1]).map(<[TabEntry]>::len), Some(1));

        // A space nobody has browsed has no answer at all — not an empty one.
        let unread = space("ws-3", false);
        assert!(data.tabs_of(&unread).is_none());
        assert_eq!(count_label(1, "tab"), "1 tab");
        assert_eq!(count_label(4, "tab"), "4 tabs");
    }

    #[test]
    fn tapping_a_row_reports_its_own_identity_and_current_expansion() {
        let (spaces, tabs) = (vec![space("ws-1", true)], tabs());
        let acts = actions(true);
        let mut logic = |_state: &mut Log| {
            tree_body(
                &TreeData {
                    spaces: &spaces,
                    tabs: &tabs,
                    browsed: None,
                    expanded_space: Some("ws-1"),
                    expanded_tab: Some("devbox"),
                    has_spaces: true,
                },
                &acts,
                TreeMode::Sheet,
            )
        };
        let mut rig = Rig::new();
        let rec = rig.frame(&mut logic, 0.0);

        rig.tap(center(rec.filled_at(CARD_RADIUS)[0]));
        assert_eq!(
            rig.state.spaces,
            vec![("ws-1".to_string(), true)],
            "the expanded space reports itself as expanded, so the caller collapses"
        );

        // The first tab row is the collapsed one.
        rig.tap(center(rec.filled_at(TAB_RADIUS)[0]));
        assert_eq!(rig.state.tabs, vec![("alpha".to_string(), false)]);

        // A pane row is the widest filled pill — wider than its own mark, and
        // filled unlike the dashed create row beside it.
        let pills = rec.filled_at(RADIUS_FULL);
        let widest = pills
            .iter()
            .copied()
            .max_by(|a, b| a.1.width.total_cmp(&b.1.width))
            .expect("the tree painted pane pills");
        rig.tap(center(widest));
        assert_eq!(rig.state.panes.len(), 1, "exactly one commit");
        assert_eq!(rig.state.panes[0].0.as_deref(), Some("ws-1"));
        assert_eq!(rig.state.panes[0].1, "devbox");
    }

    #[test]
    fn holding_a_row_renames_it_and_never_also_taps_it() {
        let (spaces, tabs) = (vec![space("ws-1", true)], tabs());
        let acts = actions(true);
        let mut logic = |_state: &mut Log| {
            tree_body(
                &TreeData {
                    spaces: &spaces,
                    tabs: &tabs,
                    browsed: None,
                    expanded_space: Some("ws-1"),
                    expanded_tab: Some("devbox"),
                    has_spaces: true,
                },
                &acts,
                TreeMode::Sheet,
            )
        };
        let mut rig = Rig::new();
        let rec = rig.frame(&mut logic, 0.0);
        let at = center(rec.filled_at(CARD_RADIUS)[0]);

        rig.hold(&mut logic, at);
        assert_eq!(rig.state.renames, vec!["space:ws-1".to_string()]);
        assert!(
            rig.state.spaces.is_empty(),
            "the recognizer resolves exactly one gesture per press"
        );
    }

    #[test]
    fn withdrawn_mutations_leave_a_browse_only_tree() {
        let (spaces, tabs) = (vec![space("ws-1", true)], tabs());
        let acts = actions(false);
        let mut logic = |_state: &mut Log| {
            tree_body(
                &TreeData {
                    spaces: &spaces,
                    tabs: &tabs,
                    browsed: None,
                    expanded_space: Some("ws-1"),
                    expanded_tab: Some("devbox"),
                    has_spaces: true,
                },
                &acts,
                TreeMode::Sheet,
            )
        };
        let mut rig = Rig::new();
        let rec = rig.frame(&mut logic, 0.0);

        for radius in [CARD_RADIUS, ADD_RADIUS, RADIUS_FULL] {
            assert!(
                rec.dashed_at(radius).is_empty(),
                "no create row survives a withdrawal"
            );
        }
        assert_eq!(rec.filled_at(CARD_RADIUS).len(), 1, "the space card stays");

        rig.hold(&mut logic, center(rec.filled_at(CARD_RADIUS)[0]));
        assert!(rig.state.renames.is_empty(), "and no hold is wired at all");
    }

    #[test]
    fn the_auto_expansion_picks_the_active_tab_and_waits_out_the_whole_reveal() {
        let tabs = tabs();
        let (key, delay) =
            auto_expand_after_cascade(&tabs, true).expect("a space with tabs auto-expands");
        assert_eq!(key, "devbox", "the active tab, not the first");
        // Three cascading children (two tabs + the create row), against the
        // theme's own height tween.
        let cascade = GlyphStagger::glyph().total_duration(3);
        assert!(delay.as_secs_f64() * 1000.0 >= cascade);

        let first = auto_expand_after_cascade(&[tab(1, "alpha", false, Vec::new())], false);
        assert_eq!(
            first.map(|(key, _)| key),
            Some("alpha".to_string()),
            "with nothing active the first tab opens"
        );
        assert!(auto_expand_after_cascade(&[], true).is_none());
    }

    #[test]
    fn reduce_motion_leaves_nothing_to_wait_for() {
        assert_eq!(cascade_settle(4, 220.0, true), Duration::ZERO);
        // Otherwise the longer of the two clocks wins.
        assert_eq!(
            cascade_settle(1, 220.0, false),
            Duration::from_secs_f64(0.220)
        );
        assert!(cascade_settle(8, 220.0, false) > Duration::from_secs_f64(0.220));
    }

    #[test]
    fn a_rows_overflow_menu_renames_it_and_creates_one_level_down() {
        let space = RowTarget::Space {
            id: "ws-1".to_string(),
            name: "one".to_string(),
        };
        let pane = RowTarget::Pane {
            id: 3,
            is_plugin: false,
            title: "zsh".to_string(),
        };
        assert_eq!(row_menu(&space).len(), 2);
        assert_eq!(row_menu(&pane).len(), 1, "a pane has nothing under it");
        assert_eq!(row_menu_choice(&space, 0), Some(RowMenuChoice::Rename));
        assert_eq!(row_menu_choice(&space, 1), Some(RowMenuChoice::NewHere));
        assert_eq!(row_menu_choice(&pane, 1), None);
    }
}
