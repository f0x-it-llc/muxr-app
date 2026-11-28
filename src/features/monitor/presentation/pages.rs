//! The `/monitor`, `/monitor/cpu` and `/monitor/processes` screens: one scaffold per route over a
//! per-mount [`MonitorController`], with the Pro guard, the host strip and the full-page states.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clean_signals_frust::use_controller;
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, Component, CrossAxisAlignment, EdgeInsets,
    Either, FlexView, Get, GetUntracked, NavigatorController, Padding, SizedBox, View,
    WindowMetrics, any, button, component, container, flexible, inflexible, on_cleanup, safe_area,
    scroll_view, spawn_local, text, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::domain::entities::epoch_millis;
use crate::features::monitor::di::new_monitor_controller;
use crate::features::monitor::presentation::controllers::{INTERVALS_MS, Link, MonitorController};
use crate::features::pro::di::expect_pro_controller;
use crate::features::pro::domain::entities::ProFeature;
use crate::features::pro::presentation::UpgradeContext;
use crate::features::pro::presentation::controller::ProController;
use crate::features::pro::presentation::widgets::upgrade_sheet::show_upgrade_sheet_for;
use crate::features::sessions::presentation::widgets::host_of;
use crate::responsive::Responsive;
use crate::routes::{MONITOR_ROUTE, MonitorView, Nav, app_nav};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::sheet::{SheetHandle, show_sheet};
use crate::ui::spacing;

use super::widgets::{
    HostStrip, dimmed, disabled_state, host_strip, offline_state, placeholder, unsupported_state,
};

pub const OVERVIEW_PLACEHOLDER: &str = "overview cards";
pub const CPU_PLACEHOLDER: &str = "cpu detail";
pub const PROCESSES_PLACEHOLDER: &str = "process table";
pub const MENU_TITLE: &str = "Monitor";
pub const PAUSE_LABEL: &str = "Pause";
pub const RESUME_LABEL: &str = "Resume";

const CLAMP_WIDTH: f64 = 640.0;
const MENU_GAP: f64 = spacing::LG;
const MENU_ROW_GAP: f64 = spacing::TIGHT;
const CURRENT_MARK: &str = "\u{25cf}";

pub fn monitor_page(
    view: MonitorView,
    overlays: NavigatorController<AppState>,
) -> impl View<AppState> {
    component(MonitorScreen { view, overlays })
}

pub struct MonitorScreen {
    view: MonitorView,
    overlays: NavigatorController<AppState>,
}

pub struct MonitorState {
    controller: Arc<MonitorController>,
    connection: Arc<ConnectionController>,
    allowed: bool,
}

#[derive(Clone)]
struct MenuChrome {
    id: u64,
    overlays: NavigatorController<AppState>,
    controller: Arc<MonitorController>,
}

thread_local! {
    /// Tagged so a cleanup withdraws only its own mount: an incoming page builds before the outgoing one tears down.
    static MONITOR_CHROME: RefCell<Option<MenuChrome>> = const { RefCell::new(None) };
}

static NEXT_CHROME_ID: AtomicU64 = AtomicU64::new(1);

/// A press with no overview mounted is ignored.
pub fn open_monitor_menu(_state: &mut AppState) {
    let Some(chrome) = MONITOR_CHROME.with(|cell| cell.borrow().clone()) else {
        return;
    };
    let controller = chrome.controller;
    show_sheet(&chrome.overlays, move |sheet| menu_body(&controller, sheet));
}

fn interval_label(interval_ms: u32) -> String {
    format!("{} s", interval_ms / 1_000)
}

fn menu_body(
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

/// Pops rather than going: a `go` would replace the retained Sessions page under the monitor.
fn redirect_to_sessions(
    nav: Nav,
    overlays: NavigatorController<AppState>,
    pro: Arc<ProController>,
) {
    spawn_local(async move {
        nav.pop();
        let unlocked = nav.clone();
        show_upgrade_sheet_for(
            &overlays,
            pro,
            ProFeature::ServerMonitor,
            UpgradeContext {
                on_unlocked: Rc::new(move || unlocked.push(MONITOR_ROUTE)),
                server_limit: None,
            },
        );
    });
}

impl Component for MonitorScreen {
    type State = MonitorState;

    fn init(&self) -> MonitorState {
        let nav = app_nav();
        let pro = expect_pro_controller();
        let connection = expect_connection_controller();
        let controller = use_controller::<MonitorController, AppFailure>(new_monitor_controller);
        let allowed = pro.can_use_untracked(ProFeature::ServerMonitor);
        if allowed {
            controller.start();
            if self.view == MonitorView::Overview {
                self.publish_menu(&controller);
            }
        } else {
            redirect_to_sessions(nav, self.overlays.clone(), pro);
        }
        MonitorState {
            controller,
            connection,
            allowed,
        }
    }

    fn build(&self, state: &mut MonitorState) -> impl View<MonitorState> {
        if !state.allowed {
            return Either::Left(SizedBox::<MonitorState>(Some(0.0), Some(0.0)));
        }
        let responsive = Responsive::of(use_context::<WindowMetrics>());
        let colors = Palette::from_theme(&active_theme());
        state.controller.observe();
        let link = state.controller.link.get();
        let body: AnyView<MonitorState> = match link {
            Link::Unsupported => {
                let (host, version) = state
                    .connection
                    .server_info
                    .get()
                    .map(|info| (host_of(&info.endpoint).to_string(), info.server_version))
                    .unwrap_or_default();
                any(unsupported_state(&host, &version))
            }
            Link::Disabled => any(disabled_state()),
            Link::Disconnected => any(offline_state()),
            Link::Live | Link::Stale { .. } => self.live_body(state, link, &colors),
        };
        let page = container(body).fill(colors.base);
        Either::Right(if responsive.wide_clamp {
            Either::Left(Align(
                Alignment::CENTER,
                SizedBox::<MonitorState>(Some(CLAMP_WIDTH), None).child(page),
            ))
        } else {
            Either::Right(page)
        })
    }
}

impl MonitorScreen {
    fn publish_menu(&self, controller: &Arc<MonitorController>) {
        let id = NEXT_CHROME_ID.fetch_add(1, Ordering::Relaxed);
        MONITOR_CHROME.with(|cell| {
            *cell.borrow_mut() = Some(MenuChrome {
                id,
                overlays: self.overlays.clone(),
                controller: Arc::clone(controller),
            });
        });
        on_cleanup(move || {
            MONITOR_CHROME.with(|cell| {
                let mut published = cell.borrow_mut();
                if published.as_ref().is_some_and(|chrome| chrome.id == id) {
                    *published = None;
                }
            });
        });
    }

    fn live_body(
        &self,
        state: &MonitorState,
        link: Link,
        colors: &Palette,
    ) -> AnyView<MonitorState> {
        let host = state.controller.host.get();
        let strip = host_strip::<MonitorState>(
            &HostStrip {
                link,
                host: host.as_ref(),
                interval_ms: state.controller.interval_ms.get(),
                paused: state.controller.paused.get(),
                now_ms: epoch_millis(),
            },
            colors,
        );
        let label = match self.view {
            MonitorView::Overview => OVERVIEW_PLACEHOLDER,
            MonitorView::Cpu => CPU_PLACEHOLDER,
            MonitorView::Processes => PROCESSES_PLACEHOLDER,
        };
        let content = safe_area(scroll_view(Padding(
            EdgeInsets::all(0.0),
            placeholder::<MonitorState>(label, colors),
        )))
        .top(false);
        any(FlexView::new(
            Axis::Vertical,
            vec![
                inflexible(strip),
                flexible(1, dimmed(content, link.is_stale())),
            ],
        )
        .cross_axis(CrossAxisAlignment::Stretch))
    }
}
