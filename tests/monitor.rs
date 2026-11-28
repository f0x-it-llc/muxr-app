//! Headless coverage of the `/monitor` routes: the Pro guard, the host strip, the full-page states
//! and the overflow menu, driven through the real router at Compact and Tablet sizes.

use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use frust::{AnyView, Component, GetUntracked, Set};
use frust_core::RenderRoot;
use frust_core::accesskit::Role;
use frust_reactive::ReactiveRuntime;
use frust_text::TextContext;
use kurbo::Size;

use muxr::connection::{CAP_HOST_VITALS, ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::monitor::di::expect_mock_monitor_repository;
use muxr::features::monitor::domain::entities::{HostInfo, VitalsDetail};
use muxr::features::monitor::presentation::controllers::INTERVALS_MS;
use muxr::features::monitor::presentation::pages::{PAUSE_LABEL, RESUME_LABEL};
use muxr::features::monitor::presentation::widgets::{
    DISABLED_BODY, DISABLED_TITLE, OFFLINE_TITLE, UNSUPPORTED_TITLE, unsupported_body,
};
use muxr::features::pro::di::{expect_pro_controller, mock_entitlement};
use muxr::features::pro::domain::entities::Entitlement;
use muxr::routes::{
    MONITOR_CPU_ROUTE, MONITOR_PROCESSES_ROUTE, MONITOR_ROUTE, MonitorView, app_nav,
    monitor_location,
};
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    metrics_at, node_center_by_label, node_labels, node_roles, node_values, serial, settle_sized,
    setup, tap,
};

const HOST: &str = "mock.example";
const OTHER_HOST: &str = "other.example";
const BRAND: &str = "muxr";
const BACK: &str = "Back";
const MENU_BUTTON: &str = "Monitor menu";
const SEEDED_IDENTITY: &str = "Arch Linux rolling \u{b7} x86_64 \u{b7} up ";
const LIVE_3S: &str = "live \u{b7} 3s";
const LIVE_5S: &str = "live \u{b7} 5s";
const RECONNECTING: &str = "reconnecting \u{b7} last sample";
const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
enum Layout {
    Compact,
    Tablet,
}

macro_rules! both_layouts {
    ($name:ident) => {
        mod $name {
            #[test]
            fn compact() {
                super::$name(super::Layout::Compact);
            }

            #[test]
            fn tablet() {
                super::$name(super::Layout::Tablet);
            }
        }
    };
}

impl Layout {
    fn size(self) -> Size {
        match self {
            Layout::Compact => Size::new(390.0, 844.0),
            Layout::Tablet => Size::new(1024.0, 768.0),
        }
    }
}

fn target(host: &str) -> ConnectTarget {
    ConnectTarget {
        host: host.to_string(),
        port: 7777,
        trust_mode: TrustMode::System,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

fn spend_cold_start() {
    static SPENT: OnceLock<()> = OnceLock::new();
    SPENT.get_or_init(|| {
        let mut throwaway = MuxrApp.init();
        let _ = MuxrApp.build(&mut throwaway);
    });
}

fn give_host_vitals() {
    let connection = expect_connection_controller();
    let mut info = connection
        .server_info
        .get_untracked()
        .expect("a connected server reports its info");
    info.capabilities = BTreeSet::from([CAP_HOST_VITALS.to_string()]);
    connection.server_info.set(Some(info));
}

fn grant_pro() {
    mock_entitlement()
        .expect("the test seam publishes the scriptable store")
        .set_entitlement(Entitlement::Pro);
    expect_pro_controller().entitlement.set(Entitlement::Pro);
}

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

struct Harness {
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    size: Size,
}

impl Harness {
    fn mount(layout: Layout) -> Self {
        spend_cold_start();
        support::reprovide_metrics(metrics_at(layout.size().width, layout.size().height));
        let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        let mut harness = Harness {
            root,
            logic: Box::new(|state: &mut AppState| AnyView::new(MuxrApp.build(state))),
            state: MuxrApp.init(),
            tcx: TextContext::new(),
            size: layout.size(),
        };
        harness.settle();
        harness
    }

    fn pro(layout: Layout) -> Self {
        let mut harness = Harness::free(layout);
        grant_pro();
        harness.settle();
        harness
    }

    fn free(layout: Layout) -> Self {
        let mut harness = Harness::mount(layout);
        expect_connection_controller().connect(target(HOST));
        give_host_vitals();
        harness.settle();
        harness.push("/sessions");
        harness
    }

    fn settle(&mut self) {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            self.size,
        );
    }

    fn pump(&mut self) {
        let runtime = ReactiveRuntime::get().expect("the reactive runtime is installed");
        for _ in 0..4 {
            runtime.pump_local();
            std::thread::sleep(Duration::from_millis(1));
            self.settle();
        }
    }

    fn push(&mut self, location: &str) {
        app_nav().push(location);
        self.settle();
        self.pump();
    }

    fn until(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; on screen: {:?}",
                self.texts()
            );
            self.pump();
        }
    }

    fn texts(&self) -> Vec<String> {
        let mut texts = node_labels(&self.root);
        texts.extend(node_values(&self.root));
        texts
    }

    fn shows(&self, text: &str) -> bool {
        self.texts().iter().any(|t| t == text)
    }

    fn shows_prefix(&self, prefix: &str) -> bool {
        self.texts().iter().any(|t| t.starts_with(prefix))
    }

    fn tap(&mut self, label: &str) {
        let point = node_center_by_label(&self.root, label)
            .unwrap_or_else(|| panic!("nothing labelled {label:?}; on screen: {:?}", self.texts()));
        tap(&mut self.root, &mut self.state, point);
        self.settle();
        self.pump();
    }

    fn has_bar(&self) -> bool {
        node_roles(&self.root).contains(&Role::TitleBar)
    }

    fn depth(&self) -> usize {
        self.state.nav.controller().depth()
    }
}

fn opens_at(interval_ms: u32) -> usize {
    expect_mock_monitor_repository()
        .vitals_opens()
        .iter()
        .filter(|detail| **detail == VitalsDetail::Full { interval_ms })
        .count()
}

fn a_pro_mount_shows_the_hosts_strip_and_the_live_interval(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::pro(layout);

    h.push(MONITOR_ROUTE);
    h.until("the live strip", |h| h.shows(LIVE_3S));

    assert!(
        h.shows_prefix(SEEDED_IDENTITY),
        "{layout:?}: {:?}",
        h.texts()
    );
    assert!(
        h.shows(HOST),
        "{layout:?}: the bar names the connected host"
    );
    assert!(h.shows(BACK), "{layout:?}");
    assert!(h.shows(MENU_BUTTON), "{layout:?}");
}

both_layouts!(a_pro_mount_shows_the_hosts_strip_and_the_live_interval);

fn a_free_mount_lands_on_sessions_with_the_upgrade_sheet_and_opens_no_stream(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::free(layout);

    h.push(MONITOR_ROUTE);
    h.until("the upgrade sheet", |h| h.shows("Server monitor"));

    let texts = h.texts();
    let position = |needle: &str| texts.iter().position(|t| t == needle);
    assert!(
        position("Server monitor") < position("Multiple servers"),
        "{layout:?}: {texts:?}"
    );
    h.tap("Not now");

    assert!(
        h.shows(BRAND),
        "{layout:?}: Sessions is underneath: {:?}",
        h.texts()
    );
    assert!(!h.shows(LIVE_3S), "{layout:?}");
    assert!(!h.shows(BACK), "{layout:?}");
    assert!(
        expect_mock_monitor_repository().vitals_opens().is_empty(),
        "{layout:?}: no stream was opened"
    );
}

both_layouts!(a_free_mount_lands_on_sessions_with_the_upgrade_sheet_and_opens_no_stream);

fn a_server_without_the_capability_gets_the_too_old_state_naming_its_muxrd(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::pro(layout);
    let connection = expect_connection_controller();
    let mut info = connection.server_info.get_untracked().expect("connected");
    info.capabilities.clear();
    connection.server_info.set(Some(info.clone()));

    h.push(MONITOR_ROUTE);

    let body = unsupported_body(HOST, &info.server_version);
    assert!(h.shows(UNSUPPORTED_TITLE), "{layout:?}: {:?}", h.texts());
    assert!(body.contains(&info.server_version));
    assert!(!h.shows_prefix("live"), "{layout:?}");
    assert!(expect_mock_monitor_repository().vitals_opens().is_empty());
}

both_layouts!(a_server_without_the_capability_gets_the_too_old_state_naming_its_muxrd);

fn a_server_with_monitoring_off_gets_the_off_state(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::pro(layout);
    expect_mock_monitor_repository().set_disabled(true);

    h.push(MONITOR_ROUTE);
    h.until("the off state", |h| h.shows(DISABLED_TITLE));

    assert!(DISABLED_BODY.contains("muxrctl"));
    assert!(!h.shows_prefix("live"), "{layout:?}");
}

both_layouts!(a_server_with_monitoring_off_gets_the_off_state);

fn losing_the_connection_shows_the_offline_state_not_the_too_old_one(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::pro(layout);
    h.push(MONITOR_ROUTE);
    h.until("the live strip", |h| h.shows(LIVE_3S));

    expect_connection_controller().disconnect();
    h.until("the offline state", |h| h.shows(OFFLINE_TITLE));

    assert!(!h.shows(UNSUPPORTED_TITLE), "{layout:?}");
    assert!(!h.shows(LIVE_3S), "{layout:?}");
}

both_layouts!(losing_the_connection_shows_the_offline_state_not_the_too_old_one);

fn a_dropped_stream_reads_stale_and_returns_to_live(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::pro(layout);
    h.push(MONITOR_ROUTE);
    h.until("the live strip", |h| h.shows(LIVE_3S));

    expect_mock_monitor_repository().set_dropped(true);
    h.until("the stale strip", |h| h.shows_prefix(RECONNECTING));

    assert!(h.shows("stale"), "{layout:?}: {:?}", h.texts());
    assert!(!h.shows(LIVE_3S), "{layout:?}");

    expect_mock_monitor_repository().set_dropped(false);
    h.until("live again", |h| h.shows(LIVE_3S));

    assert!(!h.shows("stale"), "{layout:?}");
}

both_layouts!(a_dropped_stream_reads_stale_and_returns_to_live);

fn the_menu_reopens_the_stream_at_the_chosen_interval_and_pauses_it(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::pro(layout);
    h.push(MONITOR_ROUTE);
    h.until("the live strip", |h| h.shows(LIVE_3S));
    let mock = expect_mock_monitor_repository();
    assert_eq!(opens_at(5_000), 0);

    h.tap(MENU_BUTTON);
    for interval in INTERVALS_MS {
        let current = format!("{} s", interval / 1_000);
        assert!(
            h.shows(&current) || h.shows(&format!("\u{25cf} {current}")),
            "{layout:?}: {current} is offered: {:?}",
            h.texts()
        );
    }
    assert!(
        h.shows("\u{25cf} 3 s"),
        "{layout:?}: the current one is marked"
    );
    h.tap("5 s");
    h.until("the 5 s stream", |h| h.shows(LIVE_5S));

    assert_eq!(opens_at(5_000), 1, "{layout:?}");
    assert_eq!(mock.open_watches(), 1, "{layout:?}");

    h.tap(MENU_BUTTON);
    h.tap(PAUSE_LABEL);
    h.until("the pause", |h| h.shows("paused"));

    assert_eq!(mock.open_watches(), 0, "{layout:?}");

    h.tap(MENU_BUTTON);
    assert!(h.shows(RESUME_LABEL), "{layout:?}");
    h.tap(RESUME_LABEL);
    h.until("the resume", |h| h.shows(LIVE_5S));
    assert_eq!(mock.open_watches(), 1, "{layout:?}");
}

both_layouts!(the_menu_reopens_the_stream_at_the_chosen_interval_and_pauses_it);

fn the_sub_views_mount_with_their_titles_and_back_returns_to_the_overview(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::pro(layout);
    h.push(MONITOR_ROUTE);
    h.until("the live strip", |h| h.shows(LIVE_3S));

    for (view, title) in [
        (MonitorView::Cpu, "cpu"),
        (MonitorView::Processes, "processes"),
    ] {
        let location = monitor_location(view);
        assert!(location.starts_with(MONITOR_ROUTE));
        h.push(location);
        h.until("the sub-view strip", |h| h.shows(LIVE_3S));

        assert!(h.shows(title), "{layout:?}: {location}: {:?}", h.texts());
        assert!(
            !h.shows(MENU_BUTTON),
            "{layout:?}: only the overview has the menu"
        );

        h.tap(BACK);

        assert!(
            h.shows(HOST),
            "{layout:?}: back on the overview: {:?}",
            h.texts()
        );
        assert!(h.shows(MENU_BUTTON), "{layout:?}");
    }
    assert_eq!(MONITOR_CPU_ROUTE, "/monitor/cpu");
    assert_eq!(MONITOR_PROCESSES_ROUTE, "/monitor/processes");
}

both_layouts!(the_sub_views_mount_with_their_titles_and_back_returns_to_the_overview);

fn a_server_switch_shows_the_new_host_and_never_the_old_one(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::pro(layout);
    h.push(MONITOR_ROUTE);
    h.until("the live strip", |h| h.shows_prefix(SEEDED_IDENTITY));
    expect_mock_monitor_repository().set_host(HostInfo {
        os_name: "Debian".to_string(),
        os_version: "13".to_string(),
        arch: "aarch64".to_string(),
        boot_unix_s: None,
        ..HostInfo::default()
    });

    expect_connection_controller().switch_to(target(OTHER_HOST), |_| {});
    give_host_vitals();
    h.until("the new host", |h| h.shows("Debian 13 \u{b7} aarch64"));

    assert!(h.shows(OTHER_HOST), "{layout:?}: {:?}", h.texts());
    assert!(!h.shows(HOST), "{layout:?}");
    assert!(!h.shows_prefix("Arch Linux"), "{layout:?}");
}

both_layouts!(a_server_switch_shows_the_new_host_and_never_the_old_one);

fn a_disconnected_navigation_to_monitor_bounces_to_connect(layout: Layout) {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount(layout);

    h.push(MONITOR_ROUTE);

    assert_eq!(h.depth(), 1, "{layout:?}: the shell was never placed");
    assert!(!h.has_bar(), "{layout:?}");
    assert!(expect_mock_monitor_repository().vitals_opens().is_empty());
}

both_layouts!(a_disconnected_navigation_to_monitor_bounces_to_connect);
