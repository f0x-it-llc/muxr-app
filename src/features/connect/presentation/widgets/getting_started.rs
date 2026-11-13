//! The getting-started info mark and its orientation dialog. Screen chrome, never a
//! staggered piece; on Tablet the paragraph sits inline in the left panel instead.

use frust::authoring::Size;
use frust::{
    Align, Alignment, ButtonStyle, EdgeInsets, IconSource, Padding, PopResult, Theme, View,
    WindowMetrics, button, inflexible, safe_area, text, use_context,
};
use frust_glyph::{glyph_card, glyph_dialog, show_glyph_dialog};

use super::{block_column, centered_row};
use crate::AppState;
use crate::links::open_external;
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::topbar::icon_button;

/// Also what `tests/connect.rs` addresses the button by.
pub const INFO_LABEL: &str = "Getting started";
/// Also how the tests tell the pushed page apart from the button that raised it.
pub const GETTING_STARTED_TITLE: &str = "Getting started with Muxr";
pub const GETTING_STARTED_BODY: &str = concat!(
    "Muxr is the mobile client for muxrd, a small open-source server you run on ",
    "your own machine beside zellij or herdr. Install muxrd there, pair this app ",
    "with it, and connect. The setup guide at muxr.app/docs.html walks through ",
    "every step.",
);
const CLOSE_LABEL: &str = "Close";
pub const OPEN_GUIDE_LABEL: &str = "Open guide";
pub const DOCS_URL: &str = "https://muxr.app/docs.html";

/// Material Symbols `info` (outlined), normalized onto the 24px design box; Apache-2.0.
const INFO_MARK: IconSource = IconSource {
    d: "M 11 17 h 2 v -6 h -2 v 6 Z m 1 -8 q 0.425 0 0.7125 -0.2875 T 13 8 q 0 -0.425 -0.2875 -0.7125 T 12 7 q -0.425 0 -0.7125 0.2875 T 11 8 q 0 0.425 0.2875 0.7125 T 12 9 Z m 0 13 q -2.075 0 -3.9 -0.7875 T 4.925 19.075 q -1.35 -1.35 -2.1375 -3.175 T 2 12 q 0 -2.075 0.7875 -3.9 T 4.925 4.925 q 1.35 -1.35 3.175 -2.1375 T 12 2 q 2.075 0 3.9 0.7875 T 19.075 4.925 q 1.35 1.35 2.1375 3.175 T 22 12 q 0 2.075 -0.7875 3.9 T 19.075 19.075 q -1.35 1.35 -3.175 2.1375 T 12 22 Z m 0 -2 q 3.35 0 5.675 -2.325 t 2.325 -5.675 q 0 -3.35 -2.325 -5.675 t -5.675 -2.325 q -3.35 0 -5.675 2.325 t -2.325 5.675 q 0 3.35 2.325 5.675 t 5.675 2.325 Z m 0 -8 Z",
    design: 24.0,
};

/// The QR affordance's own margin, so the two corners read as a pair.
const MARGIN: f64 = 20.0;
const BOX: f64 = 46.0;
const MARK: f64 = 22.0;

pub(crate) fn info_affordance<S: 'static>(
    overlays: &frust::NavigatorController<AppState>,
) -> impl View<S> {
    let overlays = overlays.clone();
    let colors = Palette::from_theme(&active_theme());
    // The window-control corner (iPadOS 26+ windowed), zero elsewhere. Read live: the
    // builder re-runs on every resize.
    let corner = use_context::<WindowMetrics>()
        .map(|metrics| metrics.insets.corner_insets.top_right)
        .filter(|corner| corner.height > 0.0);
    let (right, top) = corner.map_or((0.0, 0.0), |c| (c.width, c.height));
    safe_area(Align(
        Alignment::TOP_RIGHT,
        Padding(
            EdgeInsets {
                top: MARGIN + top,
                right: MARGIN + right,
                bottom: MARGIN,
                left: MARGIN,
            },
            icon_button(INFO_MARK, INFO_LABEL, move |_state: &mut S| {
                open_getting_started(&overlays);
            })
            .ink(colors.fg_muted)
            .icon_size(MARK)
            .box_size(Size::new(BOX, BOX))
            .radius(BOX / 2.0),
        ),
    ))
}

/// `text` carries one style per run, so each colour is its own run.
pub(crate) fn tablet_headline<S: 'static>(theme: &Theme, colors: &Palette) -> impl View<S> {
    let run = |copy: &str, ink| {
        inflexible(text(copy).style(inked(&theme.type_scale.headline_small, ink)))
    };
    block_column::<S, _>((
        text("attach to a").style(inked(&theme.type_scale.headline_small, colors.fg)),
        centered_row::<S>(vec![
            run("herdr", colors.amber),
            run(" or", colors.fg),
            run(" zellij", colors.amber),
            run(" session", colors.fg),
        ]),
    ))
}

pub(crate) fn getting_started_card<S: 'static>(theme: &Theme, colors: &Palette) -> impl View<S> {
    glyph_card::<S>()
        .desc(
            text(GETTING_STARTED_BODY).style(inked(&theme.type_scale.body_small, colors.fg_muted)),
        )
        .footer(Align(
            Alignment::TOP_LEFT,
            button(OPEN_GUIDE_LABEL, |_state: &mut S| open_external(DOCS_URL))
                .style(ButtonStyle::Secondary)
                .small(),
        ))
}

pub(crate) fn open_getting_started(overlays: &frust::NavigatorController<AppState>) {
    let page_nav = overlays.clone();
    show_glyph_dialog(
        overlays,
        move || {
            let close_nav = page_nav.clone();
            let guide_nav = page_nav.clone();
            glyph_dialog()
                .title(GETTING_STARTED_TITLE)
                .body(GETTING_STARTED_BODY)
                .action(
                    button(CLOSE_LABEL, move |_state: &mut AppState| {
                        close_nav.pop();
                    })
                    .style(ButtonStyle::Ghost)
                    .small(),
                )
                .action(
                    button(OPEN_GUIDE_LABEL, move |_state: &mut AppState| {
                        open_external(DOCS_URL);
                        guide_nav.pop();
                    })
                    .style(ButtonStyle::Primary)
                    .small(),
                )
        },
        |_state: &mut AppState, _result: PopResult| {},
    );
}
