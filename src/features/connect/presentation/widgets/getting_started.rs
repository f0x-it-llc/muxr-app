//! The screen's getting-started affordance: an info mark pinned to the
//! top-trailing safe area that raises a one-paragraph orientation dialog —
//! Muxr is the client, `muxrd` is the server you run — with the setup guide
//! one action away. A store-front visitor lands here with no server and no
//! idea that one is theirs to install; this is the screen's answer.
//!
//! Screen chrome, never a staggered piece (see [`super`]'s module docs), so
//! the splash never paints it. The dialog is raised on the overlay host's
//! navigator, like every other dialog this screen owns. On Tablet the mark is
//! not mounted: the paragraph sits inline in the left panel instead.

use frust::authoring::Size;
use frust::{
    Align, Alignment, AnyView, ButtonStyle, EdgeInsets, IconSource, Padding, PopResult, Theme,
    WindowMetrics, any, button, inflexible, safe_area, text, use_context,
};
use frust_glyph::{glyph_card, glyph_dialog, show_glyph_dialog};

use super::{block_column, centered_row};
use crate::AppState;
use crate::links::open_external;
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::topbar::icon_button;

/// The affordance's accessible name — what a screen reader announces, and
/// what `tests/connect.rs` addresses the button by.
pub const INFO_LABEL: &str = "Getting started";
/// The dialog's title — also how the tests tell the pushed page apart from
/// the button that raised it.
pub const GETTING_STARTED_TITLE: &str = "Getting started with Muxr";
/// The orientation paragraph — the dialog's body, and the Tablet panel's
/// inline card text.
pub const GETTING_STARTED_BODY: &str = concat!(
    "Muxr is the mobile client for muxrd, a small open-source server you run on ",
    "your own machine beside zellij or herdr. Install muxrd there, pair this app ",
    "with it, and connect. The setup guide at muxr.app/docs.html walks through ",
    "every step.",
);
const CLOSE_LABEL: &str = "Close";
pub const OPEN_GUIDE_LABEL: &str = "Open guide";
/// Where the guide action lands: the public setup guide.
pub const DOCS_URL: &str = "https://muxr.app/docs.html";

/// Material Symbols `info` (outlined), normalized onto the icon catalog's
/// 24px design box by frust's own `scripts/gen_icons.py` — the catalog's
/// starter set carries no info mark. Apache-2.0, like the rest of the set.
const INFO_MARK: IconSource = IconSource {
    d: "M 11 17 h 2 v -6 h -2 v 6 Z m 1 -8 q 0.425 0 0.7125 -0.2875 T 13 8 q 0 -0.425 -0.2875 -0.7125 T 12 7 q -0.425 0 -0.7125 0.2875 T 11 8 q 0 0.425 0.2875 0.7125 T 12 9 Z m 0 13 q -2.075 0 -3.9 -0.7875 T 4.925 19.075 q -1.35 -1.35 -2.1375 -3.175 T 2 12 q 0 -2.075 0.7875 -3.9 T 4.925 4.925 q 1.35 -1.35 3.175 -2.1375 T 12 2 q 2.075 0 3.9 0.7875 T 19.075 4.925 q 1.35 1.35 2.1375 3.175 T 22 12 q 0 2.075 -0.7875 3.9 T 19.075 19.075 q -1.35 1.35 -3.175 2.1375 T 12 22 Z m 0 -2 q 3.35 0 5.675 -2.325 t 2.325 -5.675 q 0 -3.35 -2.325 -5.675 t -5.675 -2.325 q -3.35 0 -5.675 2.325 t -2.325 5.675 q 0 3.35 2.325 5.675 t 5.675 2.325 Z m 0 -8 Z",
    design: 24.0,
};

/// Inset from the screen's safe area — the QR affordance's own margin, so the
/// two corners read as a pair. Bespoke placement geometry, not a spacing step.
const MARGIN: f64 = 20.0;
/// The tap target, clearing the 44px platform minimum, and the mark inside it.
const BOX: f64 = 46.0;
const MARK: f64 = 22.0;

/// The muted info mark, pinned to the screen's top-trailing safe area. A tap
/// raises [`open_getting_started`]'s dialog over `overlays`.
pub(crate) fn info_affordance<S: 'static>(
    overlays: &frust::NavigatorController<AppState>,
) -> AnyView<S> {
    let overlays = overlays.clone();
    let colors = Palette::from_theme(&active_theme());
    // The window-control corner (iPadOS 26+ windowed); zero everywhere else. A
    // live read on purpose: the screen's builder re-runs on every resize, so
    // the corner never goes stale. Both axes shift the mark (width trailing,
    // height top), mirroring the app bar's width shift and staying clear of a
    // wider control.
    let corner = use_context::<WindowMetrics>()
        .map(|metrics| metrics.insets.corner_insets.top_right)
        .filter(|corner| corner.height > 0.0);
    let (right, top) = corner.map_or((0.0, 0.0), |c| (c.width, c.height));
    any(safe_area(Align(
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
    )))
}

/// The Tablet panel's headline in the display face, the backend names in the
/// accent. `text` carries one style per run, so each colour is its own run.
pub(crate) fn tablet_headline<S: 'static>(theme: &Theme, colors: &Palette) -> AnyView<S> {
    let run = |copy: &str, ink| {
        inflexible(any(
            text(copy).style(inked(&theme.type_scale.headline_small, ink))
        ))
    };
    any(block_column::<S>(vec![
        any(text("attach to a").style(inked(&theme.type_scale.headline_small, colors.fg))),
        any(centered_row::<S>(vec![
            run("herdr", colors.amber),
            run(" or", colors.fg),
            run(" zellij", colors.amber),
            run(" session", colors.fg),
        ])),
    ]))
}

/// The Tablet panel's inline orientation: the dialog's paragraph in a Glyph
/// card with **Open guide** under it, through the same link seam.
pub(crate) fn getting_started_card<S: 'static>(theme: &Theme, colors: &Palette) -> AnyView<S> {
    any(glyph_card::<S>()
        .desc(
            text(GETTING_STARTED_BODY).style(inked(&theme.type_scale.body_small, colors.fg_muted)),
        )
        .footer(Align(
            Alignment::TOP_LEFT,
            button(OPEN_GUIDE_LABEL, |_state: &mut S| open_external(DOCS_URL))
                .style(ButtonStyle::Secondary)
                .small(),
        )))
}

/// Push the orientation dialog as a page of `overlays`: one paragraph, a
/// ghost **Close**, and **Open guide**, which hands [`DOCS_URL`] to the
/// device's browser and closes the dialog behind it.
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
                .action(any(button(CLOSE_LABEL, move |_state: &mut AppState| {
                    close_nav.pop();
                })
                .style(ButtonStyle::Ghost)
                .small()))
                .action(any(button(
                    OPEN_GUIDE_LABEL,
                    move |_state: &mut AppState| {
                        open_external(DOCS_URL);
                        guide_nav.pop();
                    },
                )
                .style(ButtonStyle::Primary)
                .small()))
        },
        |_state: &mut AppState, _result: PopResult| {},
    );
}
