//! The font-family picker: [`super::pickers`]' row idiom, widened for a
//! catalog that is 30-odd entries long — two sections (installed faces, then
//! the downloadable catalog) over one scrolling body, title and caption
//! pinned above it. `on_select` fires for an installed row (persist and
//! close); `on_download` for an available one — **a download never selects**,
//! it only makes the row selectable, so the sheet stays open under it.

use std::rc::Rc;

use frust::authoring::Color;
use frust::icons::CHECK;
use frust::{
    AnyView, EdgeInsets, GestureDetector, Padding, SizedBox, any, flexible, icon, inflexible, text,
};

use crate::AppState;
use crate::fonts::catalog::{CatalogStatus, FontState};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::pickers::{CHECK_SIZE, PICK_BLEED, PICK_PAD_X, PICK_PAD_Y, PickerKind, bleeding_scroll};
use super::rows::{
    GROUP_PAD_BOTTOM, GROUP_PAD_TOP, ROW_GAP, ROW_LINE_GAP, column, h_gap, rounded_fill, row,
};

/// The sheet's caption, under its title.
const FONT_SHEET_CAPTION: &str = "Downloads are kept on this device.";
const FONT_SECTION_INSTALLED: &str = "installed";
const FONT_SECTION_AVAILABLE: &str = "available";
/// A bundled face's meta line.
pub(in super::super) const FONT_META_BUILT_IN: &str = "built-in";
/// …and a downloaded one's, once installed.
pub(in super::super) const FONT_META_DOWNLOADED: &str = "downloaded";
/// The trailing affordance an available row carries, per state — Downloading
/// swaps this text for [`font_row_progress`]'s bar/dots loader instead.
pub(super) const FONT_ACTION_DOWNLOAD: &str = "download";
pub(super) const FONT_ACTION_RETRY: &str = "retry";
/// The Downloading row's determinate progress bar width. An original,
/// hand-picked value, not on the shared spacing scale.
const FONT_ROW_PROGRESS_WIDTH: f64 = 56.0;
/// What the `available` section says with no catalog behind it. Never a
/// toast: a catalog nobody can reach is not something the user did.
pub(super) const FONT_CATALOG_LOADING: &str = "loading\u{2026}";
pub(super) const FONT_CATALOG_UNAVAILABLE: &str = "Catalog unavailable offline.";
pub(super) const FONT_CATALOG_STALE: &str = "Showing the saved catalog.";

const FONT_CAPTION_GAP: f64 = spacing::BASE;

/// One row of [`font_picker_sheet`]: a font, under whatever name the catalog
/// calls it.
pub(in super::super) struct FontRow {
    /// The **family** — what a tap persists, and what a download installs.
    /// Never printed (see [`Self::display`]).
    pub(in super::super) family: String,
    /// What the row prints: the catalog's `displayName`, which differs from
    /// the family for most Nerd Fonts.
    pub(in super::super) display: String,
    /// The dim second line — `built-in`, or `OFL-1.1 · 2.4 MB`.
    pub(in super::super) meta: String,
    /// Where this font stands on the device. Every installed-section row is
    /// [`FontState::Installed`]; an available-section row carries its own
    /// download state.
    pub(in super::super) state: FontState,
    /// While [`Self::state`] is [`FontState::Downloading`]: the live
    /// received-fraction when the entry's total size is known, `None` for an
    /// indeterminate download. Unused otherwise.
    pub(in super::super) download_progress: Option<f64>,
}

/// Everything the font sheet renders — assembled by `pages` from the
/// installed faces, the catalog's signals and the stored preference.
pub(in super::super) struct FontPicker {
    /// The faces this device can already shape with, bundled first.
    pub(in super::super) installed: Vec<FontRow>,
    /// The catalog's remaining entries, in manifest order.
    pub(in super::super) available: Vec<FontRow>,
    /// The persisted family, so exactly one installed row reads as chosen.
    pub(in super::super) selected: String,
    /// Where the catalog itself stands.
    pub(in super::super) status: CatalogStatus,
    /// Whether the catalog on screen came from the offline cache.
    pub(in super::super) stale: bool,
    /// The scroll region's height cap, from the window's own metrics. `None`
    /// (every headless suite) leaves the region unbounded.
    pub(in super::super) max_height: Option<f64>,
}

/// The tap handler shared by every row of one font sheet.
type OnFont = Rc<dyn Fn(&mut AppState, String)>;

pub(in super::super) fn font_picker_sheet(
    picker: &FontPicker,
    colors: &Palette,
    on_select: impl Fn(&mut AppState, String) + 'static,
    on_download: impl Fn(&mut AppState, String) + 'static,
) -> AnyView<AppState> {
    let theme = active_theme();
    let on_select: OnFont = Rc::new(on_select);
    let on_download: OnFont = Rc::new(on_download);

    let head = vec![
        any(text(PickerKind::FontFamily.title())
            .style(inked(&theme.type_scale.title_small_emphasized, colors.fg))),
        any(SizedBox::<AppState>(None, Some(ROW_LINE_GAP))),
        any(text(FONT_SHEET_CAPTION).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
        any(SizedBox::<AppState>(None, Some(FONT_CAPTION_GAP))),
    ];

    let mut body = vec![font_section_label(FONT_SECTION_INSTALLED, colors)];
    let last_installed = picker.installed.len().saturating_sub(1);
    for (index, row) in picker.installed.iter().enumerate() {
        let selected = row.family == picker.selected;
        let on_select = Rc::clone(&on_select);
        let family = row.family.clone();
        body.push(font_pick_row(
            row,
            selected,
            index < last_installed,
            colors,
            move |state: &mut AppState| on_select(state, family.clone()),
        ));
    }

    body.push(font_section_label(FONT_SECTION_AVAILABLE, colors));
    if let Some(note) = catalog_note(picker) {
        body.push(any(Padding(
            EdgeInsets::symmetric(PICK_PAD_X, PICK_PAD_Y),
            text(note).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        )));
    }
    let last_available = picker.available.len().saturating_sub(1);
    for (index, row) in picker.available.iter().enumerate() {
        let on_download = Rc::clone(&on_download);
        let family = row.family.clone();
        body.push(font_pick_row(
            row,
            false,
            index < last_available,
            colors,
            move |state: &mut AppState| on_download(state, family.clone()),
        ));
    }

    let scroller = bleeding_scroll(body);
    let scroll_body: AnyView<AppState> = match picker.max_height {
        // A fixed height rather than a flex share: the sheet body's own
        // constraint is unbounded in height, and a `flexible` child under an
        // unbounded main axis divides zero free space — the tight-constraint
        // trap `examples/glyph-catalog`'s device gate documents.
        Some(height) => any(SizedBox::<AppState>(None, Some(height)).child(scroller)),
        None => any(scroller),
    };

    let mut children = head;
    children.push(scroll_body);
    any(column(children))
}

/// The one-line note the `available` section leads with, if any.
fn catalog_note(picker: &FontPicker) -> Option<&'static str> {
    match picker.status {
        CatalogStatus::Idle | CatalogStatus::Loading => Some(FONT_CATALOG_LOADING),
        CatalogStatus::Unavailable => Some(FONT_CATALOG_UNAVAILABLE),
        CatalogStatus::Ready if picker.stale => Some(FONT_CATALOG_STALE),
        CatalogStatus::Ready => None,
    }
}

/// A section heading inside the sheet — the screen's own `.set-group-lbl`
/// treatment, over `AppState` and without the row gutter (the sheet supplies
/// its own).
fn font_section_label(label: &str, colors: &Palette) -> AnyView<AppState> {
    let theme = active_theme();
    any(Padding(
        EdgeInsets {
            left: PICK_PAD_X,
            top: GROUP_PAD_TOP,
            right: PICK_PAD_X,
            bottom: GROUP_PAD_BOTTOM,
        },
        text(label.to_uppercase()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    ))
}

/// One font row: the display name over its meta line, a trailing `✓` when
/// chosen, and otherwise the row's own download affordance. The selected row
/// fills `--amber-faint` past the sheet's gutter exactly as
/// `super::pickers::pick_row` does, so the two sheets stay one pattern.
fn font_pick_row(
    entry: &FontRow,
    selected: bool,
    ruled: bool,
    colors: &Palette,
    on_tap: impl Fn(&mut AppState) + 'static,
) -> AnyView<AppState> {
    let theme = active_theme();
    let ink = if selected { colors.amber } else { colors.fg };
    let body = column(vec![
        any(text(entry.display.clone()).style(inked(&theme.type_scale.label_large, ink))),
        any(SizedBox::<AppState>(None, Some(ROW_LINE_GAP))),
        any(text(entry.meta.clone()).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
    ]);
    let mut children = vec![flexible(1, any(body))];
    if selected {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(icon(CHECK).size(CHECK_SIZE).color(colors.amber)));
    } else if entry.state == FontState::Downloading {
        children.push(h_gap(ROW_GAP));
        children.push(font_row_progress(entry.download_progress));
    } else if let Some((action, tone)) = font_row_action(entry.state, colors) {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(
            text(action).style(inked(&theme.type_scale.label_small, tone)),
        ));
    }

    let fill = if selected {
        colors.amber_faint
    } else {
        Color::TRANSPARENT
    };
    let mut box_ = rounded_fill(
        Padding(EdgeInsets::symmetric(PICK_PAD_X, PICK_PAD_Y), row(children)),
        fill,
        theme.shape.small,
    );
    if selected {
        box_ = box_.bleed(PICK_BLEED);
    }
    if ruled {
        box_ = box_.bottom_rule(colors.rule);
    }
    any(GestureDetector(box_).on_tap(on_tap))
}

/// The trailing affordance a non-selected row carries, and its ink — `None`
/// for an installed row simply not chosen (its own name is the whole
/// affordance) and for a downloading one, rendered through
/// [`font_row_progress`] instead. Deliberately **text**, not a vector: the
/// app has no download glyph in `frust::icons`, and a font glyph is never
/// chrome here.
fn font_row_action(state: FontState, colors: &Palette) -> Option<(&'static str, Color)> {
    match state {
        FontState::Installed | FontState::Downloading => None,
        FontState::Available => Some((FONT_ACTION_DOWNLOAD, colors.amber)),
        FontState::Failed => Some((FONT_ACTION_RETRY, colors.error)),
    }
}

/// The Downloading row's trailing affordance: a determinate `progress` bar
/// when the entry's total size is known, else the indeterminate
/// `dots_loader` — real Glyph components in place of plain text, per Ed's
/// standing directive. Either way the row's height is unchanged: both are
/// shorter than the two-line body beside them.
fn font_row_progress(fraction: Option<f64>) -> frust::FlexChild<AppState> {
    match fraction {
        Some(fraction) => inflexible(
            SizedBox::<AppState>(Some(FONT_ROW_PROGRESS_WIDTH), None)
                .child(frust_glyph::progress(fraction)),
        ),
        None => inflexible(frust_glyph::dots_loader()),
    }
}
