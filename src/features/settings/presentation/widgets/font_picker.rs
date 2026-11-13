//! The font-family sheet: installed faces, then the downloadable catalog. A download never
//! selects; it only makes the row selectable, so the sheet stays open under it.

use std::rc::Rc;

use frust::authoring::Color;
use frust::icons::CHECK;
use frust::{
    EdgeInsets, Either, GestureDetector, Padding, SizedBox, View, any, flexible, icon, inflexible,
    text,
};

use crate::AppState;
use crate::fonts::catalog::{CatalogStatus, FontState};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::pickers::{CHECK_SIZE, PICK_BLEED, PICK_PAD_X, PICK_PAD_Y, PickerKind, bleeding_scroll};
use super::rows::{
    GROUP_PAD_BOTTOM, GROUP_PAD_TOP, ROW_GAP, ROW_LINE_GAP, centered_row, h_gap, rounded_fill,
    stretch_column,
};

const FONT_SHEET_CAPTION: &str = "Downloads are kept on this device.";
const FONT_SECTION_INSTALLED: &str = "installed";
const FONT_SECTION_AVAILABLE: &str = "available";
pub(in super::super) const FONT_META_BUILT_IN: &str = "built-in";
pub(in super::super) const FONT_META_DOWNLOADED: &str = "downloaded";
pub(super) const FONT_ACTION_DOWNLOAD: &str = "download";
pub(super) const FONT_ACTION_RETRY: &str = "retry";
const FONT_ROW_PROGRESS_WIDTH: f64 = 56.0;
pub(super) const FONT_CATALOG_LOADING: &str = "loading\u{2026}";
pub(super) const FONT_CATALOG_UNAVAILABLE: &str = "Catalog unavailable offline.";
pub(super) const FONT_CATALOG_STALE: &str = "Showing the saved catalog.";

const FONT_CAPTION_GAP: f64 = spacing::BASE;

pub(in super::super) struct FontRow {
    /// The family a tap persists and a download installs; never printed.
    pub(in super::super) family: String,
    pub(in super::super) display: String,
    pub(in super::super) meta: String,
    pub(in super::super) state: FontState,
    /// The received fraction while downloading; `None` when the total size is unknown.
    pub(in super::super) download_progress: Option<f64>,
}

pub(in super::super) struct FontPicker {
    pub(in super::super) installed: Vec<FontRow>,
    pub(in super::super) available: Vec<FontRow>,
    pub(in super::super) selected: String,
    pub(in super::super) status: CatalogStatus,
    pub(in super::super) stale: bool,
    /// `None` leaves the scroll region unbounded.
    pub(in super::super) max_height: Option<f64>,
}

type OnFont = Rc<dyn Fn(&mut AppState, String)>;

pub(in super::super) fn font_picker_sheet<S, D>(
    picker: &FontPicker,
    colors: &Palette,
    on_select: S,
    on_download: D,
) -> impl View<AppState> + use<S, D>
where
    S: Fn(&mut AppState, String) + 'static,
    D: Fn(&mut AppState, String) + 'static,
{
    let theme = active_theme();
    let on_select: OnFont = Rc::new(on_select);
    let on_download: OnFont = Rc::new(on_download);

    let mut body = vec![any(font_section_label(FONT_SECTION_INSTALLED, colors))]; // erasure: keep accumulator into Vec<AnyView>
    let last_installed = picker.installed.len().saturating_sub(1);
    for (index, row) in picker.installed.iter().enumerate() {
        let selected = row.family == picker.selected;
        let on_select = Rc::clone(&on_select);
        let family = row.family.clone();
        body.push(any(font_pick_row(
            row,
            selected,
            index < last_installed,
            colors,
            move |state: &mut AppState| on_select(state, family.clone()),
        )));
    }

    body.push(any(font_section_label(FONT_SECTION_AVAILABLE, colors)));
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
        body.push(any(font_pick_row(
            row,
            false,
            index < last_available,
            colors,
            move |state: &mut AppState| on_download(state, family.clone()),
        )));
    }

    let scroller = bleeding_scroll(body);
    let scroll_body = match picker.max_height {
        // A fixed height, not a flex share: the sheet body is unbounded in height, so a
        // `flexible` child there divides zero free space.
        Some(height) => Either::Left(SizedBox::<AppState>(None, Some(height)).child(scroller)),
        None => Either::Right(scroller),
    };

    stretch_column((
        text(PickerKind::FontFamily.title())
            .style(inked(&theme.type_scale.title_small_emphasized, colors.fg)),
        SizedBox::<AppState>(None, Some(ROW_LINE_GAP)),
        text(FONT_SHEET_CAPTION).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        SizedBox::<AppState>(None, Some(FONT_CAPTION_GAP)),
        scroll_body,
    ))
}

fn catalog_note(picker: &FontPicker) -> Option<&'static str> {
    match picker.status {
        CatalogStatus::Idle | CatalogStatus::Loading => Some(FONT_CATALOG_LOADING),
        CatalogStatus::Unavailable => Some(FONT_CATALOG_UNAVAILABLE),
        CatalogStatus::Ready if picker.stale => Some(FONT_CATALOG_STALE),
        CatalogStatus::Ready => None,
    }
}

fn font_section_label(label: &str, colors: &Palette) -> impl View<AppState> {
    let theme = active_theme();
    Padding(
        EdgeInsets {
            left: PICK_PAD_X,
            top: GROUP_PAD_TOP,
            right: PICK_PAD_X,
            bottom: GROUP_PAD_BOTTOM,
        },
        text(label.to_uppercase()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    )
}

fn font_pick_row(
    entry: &FontRow,
    selected: bool,
    ruled: bool,
    colors: &Palette,
    on_tap: impl Fn(&mut AppState) + 'static,
) -> impl View<AppState> {
    let theme = active_theme();
    let ink = if selected { colors.amber } else { colors.fg };
    let body = stretch_column((
        text(entry.display.clone()).style(inked(&theme.type_scale.label_large, ink)),
        SizedBox::<AppState>(None, Some(ROW_LINE_GAP)),
        text(entry.meta.clone()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    ));
    let mut children = vec![flexible(1, body)];
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
        Padding(
            EdgeInsets::symmetric(PICK_PAD_X, PICK_PAD_Y),
            centered_row(children),
        ),
        fill,
        theme.shape.small,
    );
    if selected {
        box_ = box_.bleed(PICK_BLEED);
    }
    if ruled {
        box_ = box_.bottom_rule(colors.rule);
    }
    GestureDetector(box_).on_tap(on_tap)
}

/// Text, not a vector: `frust::icons` has no download glyph.
fn font_row_action(state: FontState, colors: &Palette) -> Option<(&'static str, Color)> {
    match state {
        FontState::Installed | FontState::Downloading => None,
        FontState::Available => Some((FONT_ACTION_DOWNLOAD, colors.amber)),
        FontState::Failed => Some((FONT_ACTION_RETRY, colors.error)),
    }
}

fn font_row_progress(fraction: Option<f64>) -> frust::FlexChild<AppState> {
    match fraction {
        Some(fraction) => inflexible(
            SizedBox::<AppState>(Some(FONT_ROW_PROGRESS_WIDTH), None)
                .child(frust_glyph::progress(fraction)),
        ),
        None => inflexible(frust_glyph::dots_loader()),
    }
}
