//! The overview's overflow menu body: the poll intervals and the pause toggle, shown in a sheet.

use std::sync::Arc;

use frust::{
    Axis, ButtonStyle, CrossAxisAlignment, FlexView, GetUntracked, SizedBox, View, button,
    inflexible, text,
};

use crate::AppState;
use crate::features::monitor::presentation::controllers::{INTERVALS_MS, MonitorController};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::sheet::SheetHandle;
use crate::ui::spacing;

pub const MENU_TITLE: &str = "Monitor";
pub const PAUSE_LABEL: &str = "Pause";
pub const RESUME_LABEL: &str = "Resume";

const MENU_GAP: f64 = spacing::LG;
const MENU_ROW_GAP: f64 = spacing::TIGHT;
const CURRENT_MARK: &str = "\u{25cf}";

fn interval_label(interval_ms: u32) -> String {
    format!("{} s", interval_ms / 1_000)
}

pub fn menu_body(
    controller: &Arc<MonitorController>,
    sheet: &SheetHandle,
) -> impl View<AppState> + use<> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let current = controller.interval_ms.get_untracked();
    let paused = controller.paused.get_untracked();
    let mut children = vec![
        inflexible(text(MENU_TITLE).style(inked(&theme.type_scale.label_large, colors.fg))),
        inflexible(SizedBox::<AppState>(None, Some(MENU_GAP))),
    ];
    for interval_ms in INTERVALS_MS {
        let label = if interval_ms == current {
            format!("{CURRENT_MARK} {}", interval_label(interval_ms))
        } else {
            interval_label(interval_ms)
        };
        let (controller, sheet) = (Arc::clone(controller), sheet.clone());
        children.push(inflexible(
            button(label, move |_: &mut AppState| {
                sheet.close();
                controller.set_interval(interval_ms);
            })
            .style(ButtonStyle::Secondary),
        ));
        children.push(inflexible(SizedBox::<AppState>(None, Some(MENU_ROW_GAP))));
    }
    let (controller, sheet) = (Arc::clone(controller), sheet.clone());
    let pause_label = if paused { RESUME_LABEL } else { PAUSE_LABEL };
    children.push(inflexible(
        button(pause_label, move |_: &mut AppState| {
            sheet.close();
            controller.set_paused(!paused);
        })
        .style(ButtonStyle::Secondary),
    ));
    FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch)
}
