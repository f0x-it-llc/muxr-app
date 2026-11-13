//! `muxrd`-backed `SessionsRepository`. Each call resolves the live client fresh; a disconnect maps to `AppFailure::Network`.
//! Mutations address a session by `Session::id` (`SessionInfo.id`, e.g. `"zellij:dev"`), never by name.
//! `ListSessions` reports zero tab/pane counts for running sessions, so counts and preview come from `GetLayout`.

use std::sync::Arc;

use tonic::Status;

use crate::agents::{AgentStatus, AgentSummary, agent_sort_key};
use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{
    CreateSessionParams, LayoutSummary, PaneSummary, RenameRequest, Session, SessionAgent,
    SessionBackend, SessionStatus, TabSummary, ThirdMetric,
};
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::net::agent_layouts::{AgentLayouts, agent_spaces, fetch_agent_layouts};
use crate::net::proto::{
    AgentList, AgentMsg, Backend, CreateSessionReq, Empty, Layout, RenameSessionReq, SessionInfo,
    SessionRef,
};
use crate::net::{MuxrClient, error};

const MAX_PREVIEW_PANES: usize = 2;

const NOT_CONNECTED: &str = "not connected to a server";

const BLANK_NAME: &str = "a session name cannot be blank";

struct LayoutDerived {
    tabs: u32,
    panes: u32,
    preview: Vec<String>,
    tabs_detail: Vec<TabSummary>,
}

pub struct GrpcSessionsRepository {
    connection: Arc<ConnectionController>,
}

impl GrpcSessionsRepository {
    pub fn new(connection: Arc<ConnectionController>) -> Self {
        Self { connection }
    }

    fn client(&self) -> Result<MuxrClient, AppFailure> {
        self.connection
            .client()
            .ok_or_else(|| AppFailure::Network(NOT_CONNECTED.to_string()))
    }

    /// `UNAUTHENTICATED` additionally fires the session-expired hook.
    fn map_status(&self, client: &MuxrClient, status: Status) -> AppFailure {
        let failure = error::from_status(&status);
        if matches!(failure, AppFailure::Auth(_)) {
            self.connection.on_session_expired_for(client);
        }
        failure
    }

    /// A `GetLayout` failure yields `None`; the caller falls back to the wire counts.
    async fn layout_for(&self, client: &MuxrClient, id: &str) -> Option<LayoutDerived> {
        let request = SessionRef {
            session: id.to_string(),
            connection_id: String::new(),
            space_id: String::new(),
        };
        match client.authed().get_layout(request).await {
            Ok(response) => Some(layout_derived_from(response.into_inner())),
            Err(status) => {
                self.map_status(client, status);
                None
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for GrpcSessionsRepository {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        let client = self.client()?;
        let response = client
            .authed()
            .list_sessions(Empty {})
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();

        let mut sessions = Vec::with_capacity(response.sessions.len());
        for info in response.sessions {
            let derived = if info.resurrectable {
                None
            } else {
                self.layout_for(&client, &info.id).await
            };
            sessions.push(map_session(info, derived));
        }
        Ok(sessions)
    }

    async fn create_session(&self, params: CreateSessionParams) -> Result<Session, AppFailure> {
        let name = params.name.trim().to_string();
        if name.is_empty() {
            return Err(AppFailure::Internal(BLANK_NAME.to_string()));
        }
        let client = self.client()?;
        let request = create_request(&name, params.backend);
        let ack = client
            .authed()
            .create_session(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)?;
        Ok(optimistic_session(
            format!("{}:{name}", params.backend.label()),
            name,
            params.backend,
        ))
    }

    async fn rename_session(&self, request: RenameRequest) -> Result<Session, AppFailure> {
        let name = request.name.trim().to_string();
        if name.is_empty() {
            return Err(AppFailure::Internal(BLANK_NAME.to_string()));
        }
        let client = self.client()?;
        let wire = RenameSessionReq {
            session: request.id.clone(),
            name: name.clone(),
        };
        let ack = client
            .authed()
            .rename_session(wire)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)?;
        // An id without `:` falls back to zellij.
        let (prefix, _) = backend_prefix(&request.id).unwrap_or(("zellij", ""));
        Ok(optimistic_session(
            format!("{prefix}:{name}"),
            name,
            backend_from_prefix(prefix),
        ))
    }

    async fn kill_session(&self, id: String) -> Result<(), AppFailure> {
        let client = self.client()?;
        let request = SessionRef {
            session: id,
            connection_id: String::new(),
            space_id: String::new(),
        };
        let ack = client
            .authed()
            .kill_session(request)
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        error::ack_result(&ack)
    }

    /// An agent the layouts cannot place keeps its row with no tab position; a failed space read is reported once.
    async fn list_agents(&self, session_id: String) -> Result<Vec<SessionAgent>, AppFailure> {
        let client = self.client()?;
        let request = SessionRef {
            session: session_id,
            connection_id: String::new(),
            space_id: String::new(),
        };
        let list = client
            .authed()
            .list_agents(request.clone())
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        let layouts = fetch_agent_layouts(&client, &request, agent_spaces(&list.agents)).await;
        if let Some(first) = layouts.failed.first() {
            let failure = self.map_status(&client, first.status.clone());
            log::warn!(
                "list_agents: {} space layout(s) unread, their agents list unplaced: {failure}",
                layouts.failed.len()
            );
        }
        Ok(map_agents(list, &layouts))
    }
}

/// Layout-derived counts win when the layout has a tab; otherwise the wire counts carry through with an empty preview.
fn map_session(info: SessionInfo, derived: Option<LayoutDerived>) -> Session {
    let backend = map_backend(info.backend);
    let status = if info.resurrectable {
        SessionStatus::Resurrectible
    } else {
        SessionStatus::Running
    };
    let third = third_metric(status, backend, &info);
    let layout = match derived {
        Some(derived) if derived.tabs > 0 => LayoutSummary {
            tabs: derived.tabs,
            panes: derived.panes,
            third,
            preview: derived.preview,
            tabs_detail: derived.tabs_detail,
            space: None,
        },
        _ => LayoutSummary {
            tabs: info.tab_count,
            panes: info.pane_count,
            third,
            preview: Vec::new(),
            tabs_detail: Vec::new(),
            space: None,
        },
    };
    Session {
        id: info.id,
        name: info.name,
        status,
        backend,
        age_secs: info.age_secs,
        has_bell: info.has_bell,
        is_current: info.is_current,
        layout,
        agents: info.agents.as_ref().map(AgentSummary::from_wire),
    }
}

fn map_agents(list: AgentList, layouts: &AgentLayouts) -> Vec<SessionAgent> {
    let mut rows: Vec<SessionAgent> = list
        .agents
        .into_iter()
        .map(|agent| {
            let position = layouts
                .tabs
                .get(&(agent.space_id.clone(), agent.tab_id))
                .map(|slot| slot.position);
            map_agent(agent, position)
        })
        .collect();
    rows.sort_by_key(|row| agent_sort_key(row.status, row.change_seq));
    rows
}

/// The wire spells an absent optional string as empty.
fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn map_agent(agent: AgentMsg, tab_position: Option<u32>) -> SessionAgent {
    SessionAgent {
        pane_id: agent.pane_id,
        tab_position,
        space_id: non_empty(agent.space_id),
        space_name: non_empty(agent.space_name),
        tab_name: agent.tab_name,
        kind: agent.kind,
        name: non_empty(agent.name),
        display_kind: non_empty(agent.display_kind),
        status: AgentStatus::from_wire(agent.status),
        title: non_empty(agent.title),
        summary: non_empty(agent.summary),
        change_seq: agent.change_seq,
    }
}

fn create_request(name: &str, backend: SessionBackend) -> CreateSessionReq {
    let wire = match backend {
        SessionBackend::Zellij => Backend::Zellij,
        SessionBackend::Herdr => Backend::Herdr,
    };
    CreateSessionReq {
        name: name.to_string(),
        layout: String::new(),
        backend: wire as i32,
    }
}

/// Unspecified or unrecognized values fall back to `Zellij`, the app's default backend.
fn map_backend(value: i32) -> SessionBackend {
    match Backend::try_from(value) {
        Ok(Backend::Herdr) => SessionBackend::Herdr,
        Ok(Backend::Zellij) | Ok(Backend::Unspecified) | Err(_) => SessionBackend::Zellij,
    }
}

/// Zellij has no space layer: its wire `space_count` is always 0.
fn third_metric(status: SessionStatus, backend: SessionBackend, info: &SessionInfo) -> ThirdMetric {
    if !status.is_live() {
        return ThirdMetric::TabsSaved(info.tab_count);
    }
    match backend {
        SessionBackend::Herdr => ThirdMetric::Spaces(info.space_count),
        SessionBackend::Zellij => ThirdMetric::Clients(info.connected_clients),
    }
}

/// Panes with blank titles still count toward `panes` but are never previewed.
fn layout_derived_from(layout: Layout) -> LayoutDerived {
    let panes = layout.tabs.iter().map(|tab| tab.panes.len() as u32).sum();
    let preview = layout
        .tabs
        .first()
        .map(|tab| {
            tab.panes
                .iter()
                .map(|pane| pane.title.as_str())
                .filter(|title| !title.trim().is_empty())
                .take(MAX_PREVIEW_PANES)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let tabs_detail = layout
        .tabs
        .iter()
        .map(|tab| TabSummary {
            index: tab.position,
            name: tab.name.clone(),
            focused: tab.active,
            has_bell: tab.has_bell,
            panes: tab
                .panes
                .iter()
                .filter(|pane| !pane.is_plugin)
                .map(|pane| PaneSummary {
                    id: pane.id,
                    title: pane.title.clone(),
                    cwd: Some(pane.cwd.clone()).filter(|cwd| !cwd.is_empty()),
                    focused: pane.is_focused,
                    floating: pane.is_floating,
                    exited: pane.exited,
                })
                .collect(),
        })
        .collect();
    LayoutDerived {
        tabs: layout.tabs.len() as u32,
        panes,
        preview,
        tabs_detail,
    }
}

fn backend_prefix(id: &str) -> Option<(&str, &str)> {
    id.split_once(':')
}

fn backend_from_prefix(prefix: &str) -> SessionBackend {
    match prefix {
        "herdr" => SessionBackend::Herdr,
        _ => SessionBackend::Zellij,
    }
}

fn optimistic_session(id: String, name: String, backend: SessionBackend) -> Session {
    let third = match backend {
        SessionBackend::Herdr => ThirdMetric::Spaces(0),
        SessionBackend::Zellij => ThirdMetric::Clients(0),
    };
    Session {
        id,
        name,
        status: SessionStatus::Running,
        backend,
        age_secs: 0,
        has_bell: false,
        is_current: false,
        layout: LayoutSummary {
            tabs: 1,
            panes: 1,
            third,
            preview: Vec::new(),
            tabs_detail: Vec::new(),
            space: None,
        },
        agents: None,
    }
}

#[cfg(test)]
mod tests {
    use frust::GetUntracked;

    use crate::net::proto::{ActionAck, PaneMsg, TabMsg};
    use crate::net::testing::MuxrdFixture;

    use super::*;

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    fn info(id: &str, name: &str, backend: i32, resurrectable: bool) -> SessionInfo {
        SessionInfo {
            name: name.to_string(),
            age_secs: 0,
            resurrectable,
            tab_count: 4,
            pane_count: 9,
            has_bell: false,
            is_current: false,
            connected_clients: 2,
            backend,
            id: id.to_string(),
            space_count: 0,
            agents: None,
        }
    }

    #[test]
    fn map_session_carries_the_agent_summary_when_the_wire_has_one() {
        let mut wire = info("herdr:a", "a", Backend::Herdr as i32, false);
        wire.agents = Some(crate::net::proto::AgentSummary {
            total: 4,
            blocked: 1,
            done: 1,
            working: 1,
            idle: 1,
            unknown: 0,
            kinds: vec!["claude".to_string(), "codex".to_string()],
        });
        let agents = map_session(wire, None).agents.expect("agents mapped");
        assert_eq!((agents.total, agents.blocked, agents.done), (4, 1, 1));
        assert_eq!((agents.working, agents.idle, agents.unknown), (1, 1, 0));
        assert_eq!(agents.kinds, ["claude", "codex"]);
    }

    #[test]
    fn map_agents_resolves_tab_positions_and_orders_by_urgency() {
        let msg = |pane_id, tab_id, space: &str, status: i32, change_seq| AgentMsg {
            pane_id,
            tab_id,
            space_id: space.to_string(),
            kind: "claude".to_string(),
            status,
            change_seq,
            ..AgentMsg::default()
        };
        let list = AgentList {
            agents: vec![
                msg(1, 10, "a", crate::net::proto::AgentStatus::Idle as i32, 5),
                msg(
                    2,
                    11,
                    "a",
                    crate::net::proto::AgentStatus::Blocked as i32,
                    1,
                ),
                msg(
                    3,
                    99,
                    "a",
                    crate::net::proto::AgentStatus::Blocked as i32,
                    9,
                ),
                msg(4, 10, "", crate::net::proto::AgentStatus::Working as i32, 2),
            ],
            ..AgentList::default()
        };
        let slot = |position: u32| crate::net::agent_layouts::TabSlot {
            tab_key: format!("t{position}"),
            position,
            name: String::new(),
        };
        let layouts = AgentLayouts {
            tabs: std::collections::HashMap::from([
                (("a".to_string(), 10), slot(0)),
                (("a".to_string(), 11), slot(3)),
                ((String::new(), 10), slot(1)),
            ]),
            failed: Vec::new(),
        };

        let rows = map_agents(list, &layouts);

        let seen: Vec<_> = rows
            .iter()
            .map(|r| (r.pane_id, r.tab_position, r.space_id.as_deref()))
            .collect();
        assert_eq!(
            seen,
            [
                (3, None, Some("a")),
                (2, Some(3), Some("a")),
                (4, Some(1), None),
                (1, Some(0), Some("a"))
            ],
            "an unplaced agent keeps its row"
        );
    }

    #[test]
    fn map_session_without_wire_agents_has_none() {
        let wire = info("zellij:a", "a", Backend::Zellij as i32, false);
        assert_eq!(map_session(wire, None).agents, None);
    }

    fn params(name: &str) -> CreateSessionParams {
        CreateSessionParams {
            name: name.to_string(),
            backend: SessionBackend::Zellij,
        }
    }

    fn pane(title: &str) -> PaneMsg {
        PaneMsg {
            id: 0,
            agent: None,
            title: title.to_string(),
            is_focused: false,
            is_floating: false,
            exited: false,
            command: String::new(),
            cwd: String::new(),
            x: 0,
            y: 0,
            rows: 0,
            cols: 0,
            is_plugin: false,
            is_fullscreen: false,
        }
    }

    fn tab(panes: Vec<PaneMsg>) -> TabMsg {
        TabMsg {
            position: 0,
            name: "main".to_string(),
            active: true,
            has_bell: false,
            panes_to_hide: 0,
            tab_id: 0,
            panes,
            fullscreen_active: false,
            floating_panes_visible: false,
        }
    }

    async fn connected_controller(
        fixture: &MuxrdFixture,
    ) -> (Arc<ConnectionController>, MuxrClient) {
        let client = fixture.connect_pinned().await;
        let connection = ConnectionController::with_live_client_for_tests(client.clone());
        (connection, client)
    }

    #[test]
    fn a_running_zellij_session_maps_its_client_count_as_the_third_metric() {
        let session = map_session(
            info("zellij:dev", "dev", Backend::Zellij as i32, false),
            None,
        );
        assert_eq!(session.id, "zellij:dev");
        assert_eq!(session.name, "dev");
        assert_eq!(session.backend, SessionBackend::Zellij);
        assert!(session.status.is_live());
        assert_eq!(session.layout.tabs, 4);
        assert_eq!(session.layout.panes, 9);
        assert_eq!(session.layout.third, ThirdMetric::Clients(2));
    }

    #[test]
    fn a_running_herdr_session_maps_its_space_count_instead() {
        let mut source = info("herdr:work", "work", Backend::Herdr as i32, false);
        source.space_count = 5;
        let session = map_session(source, None);
        assert_eq!(session.backend, SessionBackend::Herdr);
        assert_eq!(session.layout.third, ThirdMetric::Spaces(5));
    }

    #[test]
    fn a_resurrectible_session_reports_its_saved_tab_count_regardless_of_backend() {
        let session = map_session(
            info("zellij:idle", "idle", Backend::Zellij as i32, true),
            None,
        );
        assert!(!session.status.is_live());
        assert_eq!(session.layout.third, ThirdMetric::TabsSaved(4));
    }

    #[test]
    fn an_unspecified_or_unknown_wire_backend_falls_back_to_zellij() {
        assert_eq!(
            map_backend(Backend::Unspecified as i32),
            SessionBackend::Zellij
        );
        assert_eq!(map_backend(999), SessionBackend::Zellij);
    }

    #[test]
    fn a_successful_layout_derivation_overrides_the_wire_counts_and_carries_its_preview() {
        let session = map_session(
            info("zellij:dev", "dev", Backend::Zellij as i32, false),
            Some(LayoutDerived {
                tabs: 1,
                panes: 2,
                preview: vec!["btop".to_string()],
                tabs_detail: Vec::new(),
            }),
        );
        assert_eq!(session.layout.tabs, 1);
        assert_eq!(session.layout.panes, 2);
        assert_eq!(session.layout.preview, vec!["btop".to_string()]);
    }

    #[test]
    fn a_zero_tab_layout_derivation_falls_back_to_the_wire_counts() {
        let session = map_session(
            info("zellij:dev", "dev", Backend::Zellij as i32, false),
            Some(LayoutDerived {
                tabs: 0,
                panes: 0,
                preview: Vec::new(),
                tabs_detail: Vec::new(),
            }),
        );
        assert_eq!(session.layout.tabs, 4);
        assert_eq!(session.layout.panes, 9);
        assert!(session.layout.preview.is_empty());
    }

    #[test]
    fn an_empty_getlayout_response_falls_back_to_the_wire_counts_end_to_end() {
        // An empty `Layout` yields `tabs: 0`, which `map_session` treats like a failed call.
        let derived = layout_derived_from(Layout { tabs: Vec::new() });
        let session = map_session(
            info("zellij:dev", "dev", Backend::Zellij as i32, false),
            Some(derived),
        );
        assert_eq!(session.layout.tabs, 4);
        assert_eq!(session.layout.panes, 9);
        assert!(session.layout.preview.is_empty());
    }

    #[test]
    fn a_two_pane_one_tab_layout_renders_its_own_counts_not_the_wires() {
        let layout = Layout {
            tabs: vec![tab(vec![pane("btop"), pane("ed@workbench:~")])],
        };
        let session = map_session(
            info("zellij:dev", "dev", Backend::Zellij as i32, false),
            Some(layout_derived_from(layout)),
        );
        assert_eq!(session.layout.tabs, 1);
        assert_eq!(session.layout.panes, 2);
    }

    #[test]
    fn create_sends_the_chosen_backend_on_the_wire() {
        let zellij = create_request("dev", SessionBackend::Zellij);
        assert_eq!(zellij.backend, Backend::Zellij as i32);
        let herdr = create_request("work", SessionBackend::Herdr);
        assert_eq!(herdr.backend, Backend::Herdr as i32);
        assert_eq!(herdr.name, "work");
    }

    #[test]
    fn age_bell_and_current_flags_map_from_the_wire() {
        let mut source = info("zellij:dev", "dev", Backend::Zellij as i32, false);
        source.age_secs = 7200;
        source.has_bell = true;
        source.is_current = true;
        let session = map_session(source, None);
        assert_eq!(session.age_secs, 7200);
        assert!(session.has_bell);
        assert!(session.is_current);
        assert_eq!(session.age_label(), "2 hours ago");
    }

    #[test]
    fn tabs_detail_carries_names_positions_focus_and_pane_flags() {
        let mut first = tab(vec![pane("btop"), pane("shell")]);
        first.name = "ops".to_string();
        first.position = 0;
        first.active = false;
        let mut second = tab(vec![pane("editor")]);
        second.name = "code".to_string();
        second.position = 1;
        second.active = true;
        second.has_bell = true;
        second.panes[0].cwd = "/srv/app".to_string();
        second.panes[0].is_focused = true;
        second.panes[0].is_floating = true;
        second.panes[0].exited = true;

        let derived = layout_derived_from(Layout {
            tabs: vec![first, second],
        });
        assert_eq!((derived.tabs, derived.panes), (2, 3));
        let detail = derived.tabs_detail;
        assert_eq!(detail.len(), 2);
        assert_eq!((detail[0].index, detail[0].name.as_str()), (0, "ops"));
        assert!(!detail[0].focused);
        assert_eq!(detail[0].panes.len(), 2);
        assert_eq!(detail[0].panes[0].cwd, None);
        assert_eq!((detail[1].index, detail[1].name.as_str()), (1, "code"));
        assert!(detail[1].focused && detail[1].has_bell);
        let pane = &detail[1].panes[0];
        assert_eq!(pane.title, "editor");
        assert_eq!(pane.cwd.as_deref(), Some("/srv/app"));
        assert!(pane.focused && pane.floating && pane.exited);
    }

    #[test]
    fn a_plugin_pane_is_left_out_of_the_tab_summary() {
        let mut bar = pane("tab-bar");
        bar.is_plugin = true;
        let derived = layout_derived_from(Layout {
            tabs: vec![tab(vec![bar, pane("shell")])],
        });
        let titles: Vec<&str> = derived.tabs_detail[0]
            .panes
            .iter()
            .map(|p| p.title.as_str())
            .collect();
        assert_eq!(titles, ["shell"]);
    }

    #[test]
    fn zero_wire_counts_with_a_three_tab_layout_yield_three_tabs() {
        // ListSessions ships zeros for live sessions, so the GetLayout counts must win.
        let mut source = info("zellij:dev", "dev", Backend::Zellij as i32, false);
        source.tab_count = 0;
        source.pane_count = 0;
        let layout = Layout {
            tabs: vec![
                tab(vec![pane("a")]),
                tab(vec![pane("b")]),
                tab(vec![pane("c")]),
            ],
        };
        let session = map_session(source, Some(layout_derived_from(layout)));
        assert_eq!(session.layout.tabs, 3);
        assert_eq!(session.layout.panes, 3);
        assert_eq!(session.layout.tabs_detail.len(), 3);
    }

    #[test]
    fn a_resurrectible_session_keeps_an_empty_tab_summary() {
        let session = map_session(
            info("zellij:idle", "idle", Backend::Zellij as i32, true),
            None,
        );
        assert!(session.layout.tabs_detail.is_empty());
        assert_eq!(session.layout.space, None);
    }

    #[test]
    fn layout_derived_from_sums_panes_across_every_tab_and_counts_tabs() {
        let layout = Layout {
            tabs: vec![
                tab(vec![pane("btop"), pane("ed@workbench:~")]),
                tab(vec![pane("logs")]),
            ],
        };
        let derived = layout_derived_from(layout);
        assert_eq!(derived.tabs, 2);
        assert_eq!(derived.panes, 3);
    }

    #[test]
    fn layout_derived_from_takes_the_first_tabs_first_two_pane_titles() {
        let layout = Layout {
            tabs: vec![tab(vec![
                pane("btop"),
                pane("ed@workbench:~"),
                pane("a third pane, dropped"),
            ])],
        };
        assert_eq!(
            layout_derived_from(layout).preview,
            vec!["btop".to_string(), "ed@workbench:~".to_string()]
        );
    }

    #[test]
    fn layout_derived_from_filters_blank_titled_panes_but_still_counts_them() {
        let layout = Layout {
            tabs: vec![tab(vec![pane(""), pane("   "), pane("btop")])],
        };
        let derived = layout_derived_from(layout);
        assert_eq!(derived.panes, 3, "blank-titled panes still count");
        assert_eq!(derived.preview, vec!["btop".to_string()]);
    }

    #[test]
    fn layout_derived_from_tolerates_a_tabless_or_paneless_layout() {
        let tabless = layout_derived_from(Layout { tabs: Vec::new() });
        assert_eq!(tabless.tabs, 0);
        assert_eq!(tabless.panes, 0);
        assert!(tabless.preview.is_empty());

        let paneless = layout_derived_from(Layout {
            tabs: vec![tab(Vec::new())],
        });
        assert_eq!(paneless.tabs, 1);
        assert_eq!(paneless.panes, 0);
        assert!(paneless.preview.is_empty());
    }

    #[test]
    fn backend_prefix_splits_the_wires_own_id_convention() {
        assert_eq!(backend_prefix("zellij:dev"), Some(("zellij", "dev")));
        assert_eq!(backend_prefix("herdr:work"), Some(("herdr", "work")));
        assert_eq!(backend_prefix("no-colon"), None);
    }

    #[test]
    fn backend_from_prefix_recognizes_both_labels_and_falls_back_to_zellij() {
        assert_eq!(backend_from_prefix("herdr"), SessionBackend::Herdr);
        assert_eq!(backend_from_prefix("zellij"), SessionBackend::Zellij);
        assert_eq!(backend_from_prefix("mystery"), SessionBackend::Zellij);
    }

    #[test]
    fn optimistic_session_mirrors_the_mocks_own_new_session_shape() {
        let session = optimistic_session(
            "zellij:staging".to_string(),
            "staging".to_string(),
            SessionBackend::Zellij,
        );
        assert_eq!(session.id, "zellij:staging");
        assert_eq!(session.name, "staging");
        assert!(session.status.is_live());
        assert_eq!(session.layout.tabs, 1);
        assert_eq!(session.layout.panes, 1);
        assert_eq!(session.layout.third, ThirdMetric::Clients(0));
        assert!(session.layout.preview.is_empty());
    }

    #[test]
    fn a_refused_ack_fails_through_the_same_net_error_path_every_mutation_uses() {
        let refused = ActionAck {
            ok: false,
            error: "read-only session".to_string(),
            info: String::new(),
        };
        let failure = error::ack_result(&refused).expect_err("a refused action fails");
        assert!(matches!(failure, AppFailure::Network(_)));
        assert!(failure.to_string().contains("read-only session"));
    }

    #[test]
    fn every_method_fails_cleanly_and_never_dials_when_disconnected() {
        let repo = GrpcSessionsRepository::new(Arc::new(ConnectionController::detached()));

        assert!(matches!(
            block_on(repo.list_sessions()),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.create_session(params("dev"))),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.rename_session(RenameRequest {
                id: "zellij:dev".to_string(),
                name: "dev-2".to_string(),
            })),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.kill_session("zellij:dev".to_string())),
            Err(AppFailure::Network(_))
        ));
    }

    #[test]
    fn create_and_rename_reject_a_blank_name_before_ever_touching_the_client() {
        let repo = GrpcSessionsRepository::new(Arc::new(ConnectionController::detached()));

        let failure = block_on(repo.create_session(params("   "))).expect_err("blank rejected");
        assert!(matches!(failure, AppFailure::Internal(_)));

        let failure = block_on(repo.rename_session(RenameRequest {
            id: "zellij:dev".to_string(),
            name: "   ".to_string(),
        }))
        .expect_err("blank rejected");
        assert!(matches!(failure, AppFailure::Internal(_)));
    }

    #[tokio::test]
    async fn an_unauthenticated_status_logs_the_session_out_through_the_shared_hook() {
        let fixture = MuxrdFixture::start().await;
        let (connection, client) = connected_controller(&fixture).await;
        assert!(connection.connected.get_untracked());

        let repo = GrpcSessionsRepository::new(Arc::clone(&connection));
        let failure = repo.map_status(&client, Status::unauthenticated("session token expired"));

        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(
            !connection.connected.get_untracked(),
            "the session-expired hook tears the connection down"
        );
    }

    #[tokio::test]
    async fn an_unauthenticated_status_from_a_replaced_client_leaves_the_connection_alone() {
        let fixture = MuxrdFixture::start().await;
        let (connection, _live) = connected_controller(&fixture).await;
        let stale = fixture.connect_pinned().await;

        let repo = GrpcSessionsRepository::new(Arc::clone(&connection));
        let failure = repo.map_status(&stale, Status::unauthenticated("session token expired"));

        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(connection.connected.get_untracked());
    }

    #[tokio::test]
    async fn a_non_auth_status_never_touches_the_connection() {
        let fixture = MuxrdFixture::start().await;
        let (connection, client) = connected_controller(&fixture).await;
        let repo = GrpcSessionsRepository::new(Arc::clone(&connection));

        let failure = repo.map_status(&client, Status::unavailable("connection refused"));

        assert!(matches!(failure, AppFailure::Network(_)));
        assert!(
            connection.connected.get_untracked(),
            "only an auth failure logs out"
        );
    }
}
