//! [`GrpcSessionsRepository`] — the real, `muxrd`-backed [`SessionsRepository`],
//! and `sessions::di::provide`'s production choice (see that module's own
//! docs for the mock/real split and its headless-test seam).
//!
//! # The live client
//!
//! Every call resolves [`ConnectionController::client`] fresh — the sanctioned
//! path to the live [`MuxrClient`] `crate::connection`'s own module docs name
//! — rather than dialing a channel of its own. `None` (disconnected) maps to
//! [`AppFailure::Network`] rather than panicking: a repository call outliving
//! its connection (a poll tick racing a teardown) is a reachable state, not a
//! bug.
//!
//! # The one sanctioned reach into `crate::connection`
//!
//! `data` stays as framework-free as `domain` everywhere else in this feature
//! (`data`'s own module doc) — this file's `Arc<ConnectionController>` field
//! is the one exception, the same shape
//! [`crate::features::settings::data::repositories::PrefsRepository`]
//! documents for its own one sanctioned `frust::spawn_blocking` reach.
//! [`ConnectionController`] is resolved exactly once, in `di::provide`
//! (composition-root wiring, where a live reactive context is guaranteed),
//! and held from then on as a plain handle — every call this file makes
//! against it (`client()`, `on_session_expired()`) is a plain method call,
//! never a fresh `use_context` lookup.
//!
//! # Id-routed mutations
//!
//! Every mutation addresses a session by [`Session::id`] — the opaque
//! backend-qualified routing key the wire calls `SessionInfo.id`
//! (`"zellij:dev"`) — never by display name. `RenameSessionReq`/`SessionRef`'s
//! own `session` field takes that key despite its name: the field predates
//! `SessionInfo.id`, but `id` is what every caller here sends.
//!
//! # Optimistic create/rename, no follow-up round trip
//!
//! `CreateSession`/`RenameSession` answer with a plain `ActionAck` — no
//! session snapshot — and `SessionsController::create`/`SessionsController::rename`
//! already discard the [`Session`] these two methods hand back and re-list
//! immediately after (`SessionsController`'s own "refresh-after-mutation
//! round trip"), so nothing production-facing ever renders their return
//! value. Rather than pay a second `ListSessions` round trip just to
//! populate one, both construct their [`Session`] optimistically from what
//! the request already establishes: a created session is created zellij
//! (mirroring [`MockSessionsRepository`](super::MockSessionsRepository)'s
//! own `new_session` — this app offers no backend picker yet), and a
//! renamed session's id is *reconciled* — the `<backend>:<name>` convention
//! [`SessionInfo`] documents lets this file recompute the id from the old
//! id's own backend prefix plus the new name, with no round trip needed to
//! learn it. Every other field on the optimistic [`Session`] is a
//! placeholder the following refresh overwrites within one rebuild.
//!
//! # Pane preview: one `GetLayout` per session
//!
//! `SessionInfo` carries every `.sess-meta` count directly (tabs, panes, the
//! third metric) but not pane titles, so [`list_sessions`](SessionsRepository::list_sessions)
//! follows every `ListSessions` answer with one `GetLayout` per *running*
//! session and takes its first tab's first two pane titles as
//! [`LayoutSummary::preview`] — mirroring what
//! [`MockSessionsRepository`](super::MockSessionsRepository)'s seed
//! hand-encodes. A resurrectible session skips the call entirely (nothing to
//! preview); a running session with no tabs, or a first tab with no panes,
//! tolerates the empty response gracefully rather than failing.
//!
//! # Failure mapping
//!
//! Every RPC failure funnels through [`crate::net::error::from_status`]; an
//! `UNAUTHENTICATED` additionally invokes
//! [`ConnectionController::on_session_expired`] — its own idempotent contract
//! absorbs a burst of calls failing at once, so this file does no extra
//! debouncing of its own. Every `ActionAck`-returning call maps through
//! [`crate::net::error::ack_result`] instead of trusting the outer `Status`
//! alone.

use std::sync::Arc;

use tonic::Status;

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{
    LayoutSummary, RenameRequest, Session, SessionBackend, SessionStatus, ThirdMetric,
};
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::net::proto::{
    Backend, CreateSessionReq, Empty, Layout, RenameSessionReq, SessionInfo, SessionRef,
};
use crate::net::{MuxrClient, error};

/// How many of a session's first tab's pane titles [`LayoutSummary::preview`]
/// carries — that entity's own doc: "Up to two pane titles the card
/// previews". Kept as a local constant rather than shared with
/// `presentation::pieces::MAX_PREVIEW_PILLS` (a `pub(super)` rendering
/// constant this data-layer file may not import): each names the same fact
/// from its own layer's own source of truth.
const MAX_PREVIEW_PANES: usize = 2;

/// [`GrpcSessionsRepository::client`]'s failure detail when nothing is
/// connected.
const NOT_CONNECTED: &str = "not connected to a server";

/// The failure detail for a blank create/rename name — this repository's own
/// enforcement point, the same contract
/// [`MockSessionsRepository`](super::MockSessionsRepository) documents (the
/// dialog that collects a name cannot submit a blank one, so this is defence
/// in depth, not a reachable UI state).
const BLANK_NAME: &str = "a session name cannot be blank";

/// The real [`SessionsRepository`], over a live `muxrd` connection. See the
/// [module docs](self).
pub struct GrpcSessionsRepository {
    connection: Arc<ConnectionController>,
}

impl GrpcSessionsRepository {
    /// A repository over the app's one [`ConnectionController`] — resolved
    /// once by the caller (`di::provide`) and held from then on.
    pub fn new(connection: Arc<ConnectionController>) -> Self {
        Self { connection }
    }

    /// The live client, or [`AppFailure::Network`] if nothing is connected.
    /// See the [module docs](self)' "The live client" section.
    fn client(&self) -> Result<MuxrClient, AppFailure> {
        self.connection
            .client()
            .ok_or_else(|| AppFailure::Network(NOT_CONNECTED.to_string()))
    }

    /// Map a gRPC status through [`crate::net::error::from_status`], and —
    /// for `UNAUTHENTICATED` specifically — additionally invoke the shared
    /// session-expired hook (see the [module docs](self)' failure-mapping
    /// section).
    fn map_status(&self, status: Status) -> AppFailure {
        let failure = error::from_status(&status);
        if matches!(failure, AppFailure::Auth(_)) {
            self.connection.on_session_expired();
        }
        failure
    }

    /// One session's pane preview: a `GetLayout` call plus
    /// [`preview_from_layout`]. A `GetLayout` failure never fails the whole
    /// list — the counts already came back from `ListSessions` — but an
    /// `UNAUTHENTICATED` status still runs [`Self::map_status`]'s side
    /// effect, the same as every other call.
    async fn preview_for(&self, client: &MuxrClient, id: &str) -> Vec<String> {
        let request = SessionRef {
            session: id.to_string(),
            connection_id: String::new(),
        };
        match client.authed().get_layout(request).await {
            Ok(response) => preview_from_layout(response.into_inner()),
            Err(status) => {
                self.map_status(status);
                Vec::new()
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
            .map_err(|status| self.map_status(status))?
            .into_inner();

        let mut sessions = Vec::with_capacity(response.sessions.len());
        for info in response.sessions {
            // An idle/resurrectible session has nothing running to preview —
            // see the module docs' "Pane preview" section.
            let preview = if info.resurrectable {
                Vec::new()
            } else {
                self.preview_for(&client, &info.id).await
            };
            sessions.push(map_session(info, preview));
        }
        Ok(sessions)
    }

    async fn create_session(&self, name: String) -> Result<Session, AppFailure> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(AppFailure::Internal(BLANK_NAME.to_string()));
        }
        let client = self.client()?;
        let request = CreateSessionReq {
            name: name.clone(),
            layout: String::new(),
            backend: Backend::Zellij as i32,
        };
        let ack = client
            .authed()
            .create_session(request)
            .await
            .map_err(|status| self.map_status(status))?
            .into_inner();
        error::ack_result(&ack)?;
        Ok(optimistic_session(
            format!("zellij:{name}"),
            name,
            SessionBackend::Zellij,
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
            .map_err(|status| self.map_status(status))?
            .into_inner();
        error::ack_result(&ack)?;
        // The reconcile: the old id's own backend prefix, kept, paired with
        // the new name — see the module docs' "Optimistic create/rename"
        // section. An id this app never minted (no `:`) falls back to
        // `zellij`, same as an unrecognized wire `Backend`.
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
        };
        let ack = client
            .authed()
            .kill_session(request)
            .await
            .map_err(|status| self.map_status(status))?
            .into_inner();
        error::ack_result(&ack)
    }
}

// ---------------------------------------------------------------------------
// proto → entity mapping (pure; unit-tested against directly constructed
// messages, no server fixture — see the module docs)
// ---------------------------------------------------------------------------

/// A `ListSessions` entry, plus its already-resolved preview, as the
/// `/sessions` card renders it.
fn map_session(info: SessionInfo, preview: Vec<String>) -> Session {
    let backend = map_backend(info.backend);
    let status = if info.resurrectable {
        SessionStatus::Resurrectible
    } else {
        SessionStatus::Running
    };
    let third = third_metric(status, backend, &info);
    Session {
        id: info.id,
        name: info.name,
        status,
        backend,
        layout: LayoutSummary {
            tabs: info.tab_count,
            panes: info.pane_count,
            third,
            preview,
        },
    }
}

/// The wire's `Backend` enum onto the domain's own [`SessionBackend`].
/// `BACKEND_UNSPECIFIED`, or a value this build doesn't recognize, falls back
/// to [`SessionBackend::Zellij`] — the domain's closed enum has no third
/// "unknown" variant to fall back to instead, and zellij is this app's
/// default backend everywhere else a choice has to be made (see
/// [`GrpcSessionsRepository::create_session`]).
fn map_backend(value: i32) -> SessionBackend {
    match Backend::try_from(value) {
        Ok(Backend::Herdr) => SessionBackend::Herdr,
        Ok(Backend::Zellij) | Ok(Backend::Unspecified) | Err(_) => SessionBackend::Zellij,
    }
}

/// The `.sess-meta` row's varying third span, chosen the same way
/// [`MockSessionsRepository`](super::MockSessionsRepository)'s seed encodes
/// it: a resurrectible session always reports its saved tab count (nothing
/// else about it is live); a running session reports its backend's own
/// per-session fact — spaces for herdr, connected clients for zellij (zellij
/// has no space layer, so its own `space_count` is always the wire's `0`).
fn third_metric(status: SessionStatus, backend: SessionBackend, info: &SessionInfo) -> ThirdMetric {
    if !status.is_live() {
        return ThirdMetric::TabsSaved(info.tab_count);
    }
    match backend {
        SessionBackend::Herdr => ThirdMetric::Spaces(info.space_count),
        SessionBackend::Zellij => ThirdMetric::Clients(info.connected_clients),
    }
}

/// [`Layout::tabs`]'s first tab's first [`MAX_PREVIEW_PANES`] pane titles —
/// the pure half of [`GrpcSessionsRepository::preview_for`]. Empty for a
/// session with no tabs, or a first tab with no panes.
fn preview_from_layout(layout: Layout) -> Vec<String> {
    layout
        .tabs
        .first()
        .map(|tab| {
            tab.panes
                .iter()
                .take(MAX_PREVIEW_PANES)
                .map(|pane| pane.title.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Split `id` (the `<backend>:<name>` convention `SessionInfo.id` documents)
/// into its backend-label prefix and the remainder, or `None` for an id this
/// app never minted (no `:` at all).
fn backend_prefix(id: &str) -> Option<(&str, &str)> {
    id.split_once(':')
}

/// The [`SessionBackend`] a `<backend>:<name>` id's own prefix names — the
/// same labels [`SessionBackend::label`] prints. Anything else (including an
/// id this app never minted) falls back to zellij, mirroring
/// [`map_backend`]'s own fallback.
fn backend_from_prefix(prefix: &str) -> SessionBackend {
    match prefix {
        "herdr" => SessionBackend::Herdr,
        _ => SessionBackend::Zellij,
    }
}

/// A freshly created/renamed session's stand-in [`Session`]: every field this
/// caller can state with confidence (id, name, backend) plus the same
/// "new session" defaults
/// [`MockSessionsRepository`](super::MockSessionsRepository)'s own
/// `new_session` ships (one tab, one pane, running, no clients/spaces yet) —
/// see the module docs' "Optimistic create/rename" section for why nothing
/// more accurate is worth a second round trip.
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
        layout: LayoutSummary {
            tabs: 1,
            panes: 1,
            third,
            preview: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use frust::GetUntracked;
    use zeroize::Zeroizing;

    use crate::connection::{ConnectTarget, TrustMode};
    use crate::net::proto::{ActionAck, PaneMsg, TabMsg};

    use super::*;

    /// Drive one repository call to completion on a current-thread runtime —
    /// no frust runtime in play, mirroring
    /// [`MockSessionsRepository`](super::super::MockSessionsRepository)'s own
    /// tests.
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
        }
    }

    fn pane(title: &str) -> PaneMsg {
        PaneMsg {
            id: 0,
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

    /// A connected mock-backed controller —
    /// [`ConnectionController::detached`] is the same `#[cfg(test)]` seam
    /// `crate::connection`'s own tests use.
    fn connected_controller() -> Arc<ConnectionController> {
        let connection = Arc::new(ConnectionController::detached());
        connection.connect(ConnectTarget {
            host: "mock.example".to_string(),
            port: 1,
            trust_mode: TrustMode::TrustOnFirstUse,
            label: "Mock".to_string(),
            offer_credential: false,
            token: Zeroizing::new(String::new()),
            remember: false,
            fingerprint: None,
        });
        connection
    }

    // -- proto → entity mapping ----------------------------------------------

    #[test]
    fn a_running_zellij_session_maps_its_client_count_as_the_third_metric() {
        let session = map_session(
            info("zellij:dev", "dev", Backend::Zellij as i32, false),
            Vec::new(),
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
        let session = map_session(source, Vec::new());
        assert_eq!(session.backend, SessionBackend::Herdr);
        assert_eq!(session.layout.third, ThirdMetric::Spaces(5));
    }

    #[test]
    fn a_resurrectible_session_reports_its_saved_tab_count_regardless_of_backend() {
        let session = map_session(
            info("zellij:idle", "idle", Backend::Zellij as i32, true),
            Vec::new(),
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
    fn the_callers_resolved_preview_carries_straight_through() {
        let session = map_session(
            info("zellij:dev", "dev", Backend::Zellij as i32, false),
            vec!["btop".to_string()],
        );
        assert_eq!(session.layout.preview, vec!["btop".to_string()]);
    }

    // -- GetLayout's pure half ------------------------------------------------

    #[test]
    fn preview_from_layout_takes_the_first_tabs_first_two_pane_titles() {
        let layout = Layout {
            tabs: vec![tab(vec![
                pane("btop"),
                pane("ed@precision:~"),
                pane("a third pane, dropped"),
            ])],
        };
        assert_eq!(
            preview_from_layout(layout),
            vec!["btop".to_string(), "ed@precision:~".to_string()]
        );
    }

    #[test]
    fn preview_from_layout_tolerates_a_tabless_or_paneless_layout() {
        assert!(preview_from_layout(Layout { tabs: Vec::new() }).is_empty());
        assert!(
            preview_from_layout(Layout {
                tabs: vec![tab(Vec::new())]
            })
            .is_empty()
        );
    }

    // -- the rename reconcile --------------------------------------------------

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

    // -- ack failure mapping ---------------------------------------------------

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

    // -- disconnected + blank-name paths, and the session-expired hook --------

    #[test]
    fn every_method_fails_cleanly_and_never_dials_when_disconnected() {
        let repo = GrpcSessionsRepository::new(Arc::new(ConnectionController::detached()));

        assert!(matches!(
            block_on(repo.list_sessions()),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.create_session("dev".to_string())),
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

        let failure = block_on(repo.create_session("   ".to_string())).expect_err("blank rejected");
        assert!(matches!(failure, AppFailure::Internal(_)));

        let failure = block_on(repo.rename_session(RenameRequest {
            id: "zellij:dev".to_string(),
            name: "   ".to_string(),
        }))
        .expect_err("blank rejected");
        assert!(matches!(failure, AppFailure::Internal(_)));
    }

    #[test]
    fn an_unauthenticated_status_logs_the_session_out_through_the_shared_hook() {
        let connection = connected_controller();
        assert!(connection.connected.get_untracked());

        let repo = GrpcSessionsRepository::new(Arc::clone(&connection));
        let failure = repo.map_status(Status::unauthenticated("session token expired"));

        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(
            !connection.connected.get_untracked(),
            "the session-expired hook tears the connection down"
        );
    }

    #[test]
    fn a_non_auth_status_never_touches_the_connection() {
        let connection = connected_controller();
        let repo = GrpcSessionsRepository::new(Arc::clone(&connection));

        let failure = repo.map_status(Status::unavailable("connection refused"));

        assert!(matches!(failure, AppFailure::Network(_)));
        assert!(
            connection.connected.get_untracked(),
            "only an auth failure logs out"
        );
    }
}
