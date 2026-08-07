//! [`GrpcTerminalRepository`] — the real, `muxrd`-backed [`TerminalRepository`],
//! and `terminal::di::provide`'s production choice (see that module's own docs
//! for the mock/real split and its headless-test seam).
//!
//! # The live client, and the one sanctioned reach into `crate::connection`
//!
//! Every call resolves [`ConnectionController::client`] fresh — the sanctioned
//! path to the live [`MuxrClient`] `crate::connection`'s own module docs name —
//! rather than dialing a channel of its own; `None` (disconnected) maps to
//! [`AppFailure::Network`] rather than panicking, since a call outliving its
//! connection is a reachable state, not a bug. The `Arc<ConnectionController>`
//! field is this feature's one `data` → `crate::connection` edge, the identical
//! shape [`GrpcSessionsRepository`](crate::features::sessions::data::repositories::GrpcSessionsRepository)
//! documents: resolved exactly once, in `di::provide` (composition-root wiring,
//! where a live reactive context is guaranteed), and held from then on as a
//! plain handle.
//!
//! # What it addresses, and how it learns it
//!
//! The four read/detach methods this trait was minted with take no session id
//! ([`TerminalRepository`]'s own "Which session, and over which connection"),
//! so this repository is [`bind`](TerminalRepository::bind)-ed instead: the
//! screen hands it the attached session's id on mount, and the *connection id*
//! the `AttachTerminal` stream minted as soon as one exists (and again on every
//! re-attach, which mints a fresh one). Both ride on every request that has a
//! field for them, which is what makes a space/tab switch land on **this**
//! client's view rather than on whichever one the relay saw last. An unbound
//! session is not an error: a bare `/terminal` has nothing to list, so the two
//! reads answer empty rather than failing a screen the user reached
//! legitimately.
//!
//! # Two calls that never reach the wire
//!
//! - **`tab_lines`** — on a live session a tab's content is the emulator's, fed
//!   by the attached stream; there is no RPC that returns scrollback, and the
//!   grid owns what is on screen. So it answers **empty**: the card's mock
//!   transcript is what those lines drive, and a live session renders the grid
//!   instead (`presentation::pages`' pane-content switch).
//! - **`detach_session`** — there is no `Detach` RPC, because detaching is
//!   client-side: it *is* dropping the `AttachTerminal` stream, which
//!   `GrpcTerminalFeed`'s own [`Drop`] does forcefully when the screen unmounts.
//!   So this validates its id (the same defence-in-depth the mock keeps) and
//!   answers `Ok`; the pop that follows is what actually detaches.
//!
//! # Tab keys are derived, not carried
//!
//! [`TabEntry`] separates three identifiers on purpose (its own docs): the
//! display `index`, the backend's `tab_id`, and `key` — the screen's own lookup
//! handle, which the sheet also *prints* (`panes — devbox`). The wire carries
//! no such handle, so [`map_tabs`] derives one per tab from its name, falling
//! back to a positional stand-in for an unnamed tab and disambiguating a
//! repeated name by its position. Uniqueness is the load-bearing half: the
//! sheet resolves the active tab by `key`, so two tabs sharing one would make
//! a pill switch to the wrong card.
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

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tonic::Status;

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{Space, TabEntry, TermLine};
use crate::features::terminal::domain::repositories::TerminalRepository;
use crate::net::proto::{Layout, PaneMsg, SessionRef, SpaceList, SwitchSpaceReq, TabTarget};
use crate::net::{MuxrClient, error};

/// [`GrpcTerminalRepository::client`]'s failure detail when nothing is
/// connected.
const NOT_CONNECTED: &str = "not connected to a server";

/// The failure detail for a switch attempted with no session bound — a bug
/// (the sheet only ever switches on a screen that carries a session), so it is
/// [`AppFailure::Internal`] rather than a retryable network one.
const NO_SESSION: &str = "no session is bound to this terminal repository";

/// The failure detail for a blank detach id — defence in depth, matching the
/// mock's own enforcement point.
const BLANK_SESSION: &str = "detach_session called with a blank id";

/// What an unnamed tab's [`TabEntry::key`] falls back to, before the 1-based
/// position it is completed with (`tab 3`).
const UNNAMED_TAB: &str = "tab";

/// The real [`TerminalRepository`], over a live `muxrd` connection. See the
/// [module docs](self).
pub struct GrpcTerminalRepository {
    connection: Arc<ConnectionController>,
    /// What every request addresses — see the [module docs](self)' "What it
    /// addresses". A `Mutex` (not a `RefCell`) because this travels behind an
    /// `Arc<dyn TerminalRepository + Send + Sync>`.
    binding: Mutex<Binding>,
}

/// The session and per-connection routing token
/// [`TerminalRepository::bind`] records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Binding {
    /// The attached session's backend-qualified id, or `None` on a bare
    /// `/terminal`.
    session: Option<String>,
    /// The `AttachTerminal` stream's minted id, echoed back for per-connection
    /// routing. Empty is the wire's own "legacy/fallback routing", which is
    /// exactly the right answer before a stream has minted one.
    connection_id: String,
}

impl GrpcTerminalRepository {
    /// A repository over the app's one [`ConnectionController`] — resolved
    /// once by the caller (`di::provide`) and held from then on. Unbound until
    /// the screen calls [`TerminalRepository::bind`].
    pub fn new(connection: Arc<ConnectionController>) -> Self {
        Self {
            connection,
            binding: Mutex::new(Binding::default()),
        }
    }

    /// The live client, or [`AppFailure::Network`] if nothing is connected.
    fn client(&self) -> Result<MuxrClient, AppFailure> {
        self.connection
            .client()
            .ok_or_else(|| AppFailure::Network(NOT_CONNECTED.to_string()))
    }

    /// The current binding. Poisoning is recovered from rather than propagated
    /// — a panic elsewhere must not make a live terminal permanently
    /// unaddressable.
    fn binding(&self) -> MutexGuard<'_, Binding> {
        self.binding.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The bound session id and routing token, or [`AppFailure::Internal`]
    /// when no session is bound — the shape every *switch* needs, as opposed
    /// to the reads, which answer empty instead.
    fn addressed(&self) -> Result<(String, String), AppFailure> {
        let binding = self.binding();
        match &binding.session {
            Some(session) => Ok((session.clone(), binding.connection_id.clone())),
            None => Err(AppFailure::Internal(NO_SESSION.to_string())),
        }
    }

    /// A [`SessionRef`] for the bound session, or `None` when nothing is bound.
    fn session_ref(&self) -> Option<SessionRef> {
        let binding = self.binding();
        binding.session.as_ref().map(|session| SessionRef {
            session: session.clone(),
            connection_id: binding.connection_id.clone(),
        })
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
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl TerminalRepository for GrpcTerminalRepository {
    fn bind(&self, session: Option<String>, connection_id: Option<String>) {
        let mut binding = self.binding();
        binding.session = session;
        binding.connection_id = connection_id.unwrap_or_default();
    }

    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
        // Nothing bound, nothing to list — a bare `/terminal` is a reachable
        // screen, not a failure (see the module docs).
        let Some(request) = self.session_ref() else {
            return Ok(Vec::new());
        };
        let client = self.client()?;
        let response = client
            .authed()
            .get_spaces(request)
            .await
            .map_err(|status| self.map_status(status))?
            .into_inner();
        Ok(map_spaces(response))
    }

    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
        let Some(request) = self.session_ref() else {
            return Ok(Vec::new());
        };
        let client = self.client()?;
        let response = client
            .authed()
            .get_layout(request)
            .await
            .map_err(|status| self.map_status(status))?
            .into_inner();
        Ok(map_tabs(response))
    }

    async fn tab_lines(&self, _key: String) -> Result<Vec<TermLine>, AppFailure> {
        // The emulator owns a live session's content — see the module docs'
        // "Two calls that never reach the wire".
        Ok(Vec::new())
    }

    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = SwitchSpaceReq {
            session,
            space_id,
            connection_id,
        };
        let ack = client
            .authed()
            .switch_space(request)
            .await
            .map_err(|status| self.map_status(status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
        let (session, connection_id) = self.addressed()?;
        let client = self.client()?;
        let request = TabTarget {
            session,
            tab_id,
            connection_id,
        };
        let ack = client
            .authed()
            .go_to_tab(request)
            .await
            .map_err(|status| self.map_status(status))?
            .into_inner();
        error::ack_result(&ack)
    }

    async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
        if id.trim().is_empty() {
            return Err(AppFailure::Internal(BLANK_SESSION.to_string()));
        }
        // No RPC: the stream's teardown *is* the detach — see the module docs'
        // "Two calls that never reach the wire".
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// proto → entity mapping (pure; unit-tested against directly constructed
// messages, no server fixture — see the module docs)
// ---------------------------------------------------------------------------

/// A `GetSpaces` answer as the sheet's `spaces` pill row.
///
/// The wire's `active` is the *per-connection* answer (the relay overrides it
/// for the connection that last switched), which is why the sheet can show one
/// selection per client rather than one per session.
fn map_spaces(list: SpaceList) -> Vec<Space> {
    list.spaces
        .into_iter()
        .map(|space| Space {
            id: space.id,
            name: space.name,
            selected: space.active,
        })
        .collect()
}

/// A `GetLayout` answer as the sheet's `tabs` pill row and the card's status
/// row.
///
/// One [`TabEntry`] per tab, in wire order. `position` is 0-indexed on the wire
/// and 1-based on the pill, and every pane fact comes from the tab's *focused*
/// pane (falling back to its first), since that is the one the card's status
/// row is describing and the one the attached stream is carrying.
fn map_tabs(layout: Layout) -> Vec<TabEntry> {
    let mut taken: Vec<String> = Vec::with_capacity(layout.tabs.len());
    layout
        .tabs
        .into_iter()
        .map(|tab| {
            let index = tab.position.saturating_add(1);
            let key = unique_key(&tab.name, index, &mut taken);
            let pane = focused_pane(&tab.panes);
            TabEntry {
                index,
                tab_id: u64::from(tab.tab_id),
                key,
                label: display_name(&tab.name, index),
                // A live pane carries a title and a working directory, and
                // nothing that corresponds to the mock's separate "name": the
                // title *is* the name a real session shows.
                pane_title: pane.map(|pane| pane.title.clone()).unwrap_or_default(),
                pane_name: pane.map(|pane| pane.title.clone()).unwrap_or_default(),
                pane_path: pane.map(|pane| pane.cwd.clone()).unwrap_or_default(),
                active: tab.active,
            }
        })
        .collect()
}

/// The pane a tab's status row describes: the focused one, or — for a tab the
/// backend reports with nothing focused — its first.
fn focused_pane(panes: &[PaneMsg]) -> Option<&PaneMsg> {
    panes
        .iter()
        .find(|pane| pane.is_focused)
        .or_else(|| panes.first())
}

/// A tab's human label: its own name, or a positional stand-in when the
/// backend reports none (`tab 3`).
fn display_name(name: &str, index: u32) -> String {
    let name = name.trim();
    if name.is_empty() {
        format!("{UNNAMED_TAB} {index}")
    } else {
        name.to_string()
    }
}

/// A [`TabEntry::key`] that no earlier tab in the same layout already took,
/// recording it in `taken` — see the [module docs](self)' "Tab keys are
/// derived".
///
/// A repeated name is completed with its own 1-based position rather than
/// renumbered from scratch, so the *first* `zsh` keeps the plain key and the
/// second reads `zsh 4` — which is also what the sheet's `panes — <key>`
/// section label then prints.
fn unique_key(name: &str, index: u32, taken: &mut Vec<String>) -> String {
    let base = display_name(name, index);
    let key = if taken.contains(&base) {
        format!("{base} {index}")
    } else {
        base
    };
    taken.push(key.clone());
    key
}

#[cfg(test)]
mod tests {
    use frust::GetUntracked;
    use zeroize::Zeroizing;

    use crate::connection::{ConnectTarget, TrustMode};
    use crate::net::proto::{ActionAck, PaneMsg, Space as WireSpace, TabMsg};

    use super::*;

    /// Drive one repository call to completion on a current-thread runtime —
    /// no frust runtime in play, mirroring the mock repository's own tests.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    /// A disconnected repository, bound to `session` — the shape every
    /// no-wire test below needs.
    fn repo(session: Option<&str>) -> GrpcTerminalRepository {
        let repo = GrpcTerminalRepository::new(Arc::new(ConnectionController::detached()));
        repo.bind(session.map(str::to_string), None);
        repo
    }

    fn wire_space(id: &str, name: &str, active: bool) -> WireSpace {
        WireSpace {
            id: id.to_string(),
            name: name.to_string(),
            active,
        }
    }

    fn pane(title: &str, cwd: &str, is_focused: bool) -> PaneMsg {
        PaneMsg {
            id: 0,
            title: title.to_string(),
            is_focused,
            is_floating: false,
            exited: false,
            command: String::new(),
            cwd: cwd.to_string(),
            x: 0,
            y: 0,
            rows: 0,
            cols: 0,
            is_plugin: false,
            is_fullscreen: false,
        }
    }

    fn tab(position: u32, tab_id: u32, name: &str, active: bool, panes: Vec<PaneMsg>) -> TabMsg {
        TabMsg {
            position,
            name: name.to_string(),
            active,
            has_bell: false,
            panes_to_hide: 0,
            tab_id,
            panes,
            fullscreen_active: false,
            floating_panes_visible: false,
        }
    }

    /// A connected mock-backed controller — [`ConnectionController::detached`]
    /// is the same `#[cfg(test)]` seam `crate::connection`'s own tests use.
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
    fn a_space_list_maps_its_id_name_and_per_connection_active_flag() {
        let spaces = map_spaces(SpaceList {
            spaces: vec![
                wire_space("ws-1", "cupline", false),
                wire_space("ws-2", "forgekit", true),
            ],
        });

        assert_eq!(spaces.len(), 2);
        assert_eq!(spaces[0].id, "ws-1");
        assert_eq!(spaces[0].name, "cupline");
        assert!(!spaces[0].selected);
        assert!(
            spaces[1].selected,
            "the wire's `active` is what the sheet paints selected"
        );
    }

    #[test]
    fn a_zellij_session_maps_its_empty_space_list_to_an_empty_pill_row() {
        assert!(map_spaces(SpaceList { spaces: Vec::new() }).is_empty());
    }

    #[test]
    fn a_layout_maps_one_tab_entry_per_tab_with_its_backend_id_and_pill_index() {
        let tabs = map_tabs(Layout {
            tabs: vec![
                tab(0, 7, "btop", false, vec![pane("htop", "/home/ed", true)]),
                tab(1, 9, "devbox", true, vec![pane("zsh", "/srv", true)]),
            ],
        });

        assert_eq!(tabs.len(), 2);
        assert_eq!(
            tabs[0].index, 1,
            "the wire counts from zero, the pill from one"
        );
        assert_eq!(tabs[0].tab_id, 7);
        assert_eq!(tabs[0].key, "btop");
        assert_eq!(tabs[0].pill_label(), "1 btop");
        assert!(!tabs[0].active);
        assert_eq!(tabs[1].index, 2);
        assert_eq!(tabs[1].tab_id, 9);
        assert!(tabs[1].active);
    }

    #[test]
    fn a_tabs_pane_facts_come_from_its_focused_pane() {
        let tabs = map_tabs(Layout {
            tabs: vec![tab(
                0,
                1,
                "dev",
                true,
                vec![
                    pane("first", "/first", false),
                    pane("ed@devbox", "/home/ed", true),
                ],
            )],
        });

        assert_eq!(tabs[0].pane_title, "ed@devbox");
        assert_eq!(tabs[0].pane_name, "ed@devbox");
        assert_eq!(tabs[0].pane_path, "/home/ed");
    }

    #[test]
    fn a_tab_with_nothing_focused_falls_back_to_its_first_pane_and_a_paneless_one_to_blanks() {
        let tabs = map_tabs(Layout {
            tabs: vec![
                tab(0, 1, "a", true, vec![pane("first", "/first", false)]),
                tab(1, 2, "b", false, Vec::new()),
            ],
        });

        assert_eq!(tabs[0].pane_title, "first");
        assert_eq!(tabs[1].pane_title, "");
        assert_eq!(tabs[1].pane_path, "");
    }

    #[test]
    fn an_unnamed_tab_takes_a_positional_label_and_key() {
        let tabs = map_tabs(Layout {
            tabs: vec![tab(2, 5, "   ", false, Vec::new())],
        });

        assert_eq!(tabs[0].key, "tab 3");
        assert_eq!(tabs[0].label, "tab 3");
        assert_eq!(tabs[0].pill_label(), "3 tab 3");
    }

    #[test]
    fn a_repeated_tab_name_keeps_a_unique_key_so_the_sheet_cannot_switch_to_the_wrong_card() {
        let tabs = map_tabs(Layout {
            tabs: vec![
                tab(0, 1, "zsh", false, Vec::new()),
                tab(1, 2, "zsh", false, Vec::new()),
                tab(2, 3, "zsh", false, Vec::new()),
            ],
        });

        let keys: Vec<&str> = tabs.iter().map(|tab| tab.key.as_str()).collect();
        assert_eq!(keys, ["zsh", "zsh 2", "zsh 3"]);
        assert_eq!(
            tabs[1].label, "zsh",
            "only the lookup key is disambiguated; the pill still reads the tab's own name"
        );
    }

    #[test]
    fn an_empty_layout_maps_to_no_tabs() {
        assert!(map_tabs(Layout { tabs: Vec::new() }).is_empty());
    }

    // -- what the binding puts on the wire ------------------------------------

    #[test]
    fn binding_rides_on_every_request_that_has_a_field_for_it() {
        let repo = GrpcTerminalRepository::new(Arc::new(ConnectionController::detached()));
        assert_eq!(repo.session_ref(), None, "unbound addresses nothing");

        repo.bind(Some("herdr:work".to_string()), Some("conn-7".to_string()));
        assert_eq!(
            repo.session_ref(),
            Some(SessionRef {
                session: "herdr:work".to_string(),
                connection_id: "conn-7".to_string(),
            })
        );
        assert_eq!(
            repo.addressed(),
            Ok(("herdr:work".to_string(), "conn-7".to_string()))
        );
    }

    #[test]
    fn a_binding_with_no_connection_id_yet_sends_the_wires_own_fallback_routing() {
        let repo = repo(Some("zellij:dev"));
        assert_eq!(
            repo.session_ref().map(|request| request.connection_id),
            Some(String::new()),
            "empty is the wire's documented legacy/fallback routing"
        );
    }

    #[test]
    fn rebinding_replaces_the_id_a_re_attached_stream_superseded() {
        let repo = repo(Some("zellij:dev"));
        repo.bind(Some("zellij:dev".to_string()), Some("conn-1".to_string()));
        repo.bind(Some("zellij:dev".to_string()), Some("conn-2".to_string()));

        assert_eq!(repo.addressed().map(|(_, id)| id), Ok("conn-2".to_string()));
    }

    // -- the two calls that never reach the wire ------------------------------

    #[test]
    fn tab_lines_answers_empty_because_the_emulator_owns_a_live_sessions_content() {
        // Disconnected on purpose: this must not dial at all, so the "not
        // connected" failure every wire-bound call answers is exactly what it
        // must *not* produce.
        let repo = repo(Some("zellij:dev"));
        assert_eq!(
            block_on(repo.tab_lines("devbox".to_string())),
            Ok(Vec::new())
        );
    }

    #[test]
    fn detach_validates_its_id_and_never_dials() {
        let repo = repo(Some("zellij:dev"));
        assert!(block_on(repo.detach_session("zellij:dev".to_string())).is_ok());
        assert!(matches!(
            block_on(repo.detach_session("   ".to_string())),
            Err(AppFailure::Internal(_))
        ));
    }

    // -- the unbound and disconnected paths -----------------------------------

    #[test]
    fn an_unbound_repository_lists_nothing_rather_than_failing_a_bare_terminal_route() {
        let repo = repo(None);
        assert_eq!(block_on(repo.list_spaces()), Ok(Vec::new()));
        assert_eq!(block_on(repo.list_tabs()), Ok(Vec::new()));
    }

    #[test]
    fn an_unbound_switch_is_an_internal_failure_rather_than_a_retryable_one() {
        let repo = repo(None);
        assert!(matches!(
            block_on(repo.switch_space("ws-1".to_string())),
            Err(AppFailure::Internal(_))
        ));
        assert!(matches!(
            block_on(repo.go_to_tab(3)),
            Err(AppFailure::Internal(_))
        ));
    }

    #[test]
    fn every_wire_bound_method_fails_cleanly_when_disconnected() {
        let repo = repo(Some("zellij:dev"));

        assert!(matches!(
            block_on(repo.list_spaces()),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.list_tabs()),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.switch_space("ws-1".to_string())),
            Err(AppFailure::Network(_))
        ));
        assert!(matches!(
            block_on(repo.go_to_tab(3)),
            Err(AppFailure::Network(_))
        ));
    }

    // -- ack + status mapping -------------------------------------------------

    #[test]
    fn a_refused_ack_fails_through_the_same_net_error_path_every_switch_uses() {
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
    fn an_unauthenticated_status_logs_the_session_out_through_the_shared_hook() {
        let connection = connected_controller();
        assert!(connection.connected.get_untracked());

        let repo = GrpcTerminalRepository::new(Arc::clone(&connection));
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
        let repo = GrpcTerminalRepository::new(Arc::clone(&connection));

        let failure = repo.map_status(Status::unavailable("connection refused"));

        assert!(matches!(failure, AppFailure::Network(_)));
        assert!(
            connection.connected.get_untracked(),
            "only an auth failure logs out"
        );
    }
}
