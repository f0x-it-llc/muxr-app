//! [`ServersSwitcherController`] — the server switcher's view model.
//!
//! **App-scoped**, built once by [`crate::features::servers::di`]: the stage
//! is raised over the shell by the shell host, and this controller owns what
//! that host reads — whether the stage is [`open`](Self::open), the strip's
//! [`faces`](Self::faces), which peer card is [`expanding`](Self::expanding)
//! into the live page, and which endpoint a switch is [`busy`](Self::busy)
//! dialing.
//!
//! **Strip order.** `faces` keeps one stable per-process order (the saved
//! list's stored order on first build, new rows appended, removed rows
//! dropped; a connect, switch or keyring lock never reorders it). The live
//! card is the switcher component's real page at its own slot, so the
//! component's *peers* are the faces that are not [`FaceKind::Live`], in
//! order. [`peer_endpoints`] is that
//! mapping and [`select`](ServersSwitcherController::select) takes the
//! component's peer index through it.
//!
//! **A switch** ([`select`](ServersSwitcherController::select)) sets
//! `expanding` first, so the skin grows the card while the work runs, then:
//! the free-tier verdict (blocked raises the paywall through the caller's
//! [`SwitchGate`], clears `expanding`, dials nothing); a row that claims no
//! credential is handed to the add-server page instead of being dialed; a
//! locked keyring gets `connect`'s page-agnostic [`KeyringUnlock`] — one
//! prompt — and the dial then goes through
//! [`KeyringUnlock::switch_to_saved`] with the keyring's token. When the
//! connection settles, `open` and `expanding` are cleared together (two
//! writes with nothing between them and the next frame), so the shell host
//! rebuilds around the new server. A failure clears `expanding` (the ramp
//! reverses, the stage stays open) and marks that face
//! [`FaceKind::Failed`]; a second tap retries.
//!
//! **A cached switch** (the target has a [`PageSnapshot`] in the last-seen map)
//! commits instead of waiting: once the select path has vetted the row it
//! publishes a [`ReconnectSeed`] (the Sessions page seeds itself from it and
//! stays on screen, actions off), closes the stage at once with the settle-close
//! semantics (no peers), dials in the
//! background. The target counts as live while that dial is in flight, so the
//! strip names it. `Connecting` shows only if the dial is still pending after
//! `banner_connecting_delay_ms`; landing then flips it to `Connected` for
//! `banner_success_ms` (a dial that lands first shows no banner), then clears it; a failure shows `Failed` until the next
//! switch or `banner_failure_ms` and marks the face failed. Every banner change
//! bumps a generation that fences the clearing timers. A second select while a
//! background dial is in flight supersedes it (the connection is torn down and
//! the new switch dials). A target with no cached page keeps the stage-held path.
//!
//! No `Nav`, overlay or other `Rc` handle is stored: the caller lends them per
//! call ([`SwitchGate`], [`lock`](ServersSwitcherController::lock)), which
//! keeps this controller `Send + Sync`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use frust::{Get, GetUntracked, NavigatorController, RwSignal, Set, spawn_local};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::connect::di::{Credentials, KeyringHandle, SavedServers};
use crate::features::connect::domain::entities::SavedServer;
use crate::features::connect::presentation::{KeyringUnlock, KeyringUnlockOutcome};
use crate::features::pro::domain::entities::ServerLimitVerdict;
use crate::features::pro::presentation::controller::ProController;
use crate::features::pro::presentation::{UpgradeContext, show_upgrade_sheet};
use crate::features::servers::di::LastSeenHandle;
use crate::features::servers::domain::entities::{FaceKind, PageSnapshot, ServerFace, faces_for};
use crate::routes::{ADD_SERVER_LOCATION, Nav, add_server_location};
use crate::ui::toasts::app_toasts;

/// Shown on a face whose credential could not be found once unlocked.
const NO_CREDENTIAL_MESSAGE: &str = "No saved credential found.";
/// Where a lock sends the app: Connect.
const LOCKED_LOCATION: &str = "/";

/// Navigation lent to a close that leaves the app disconnected.
type Landing = Rc<dyn Fn(&str)>;

thread_local! {
    /// How a close that leaves the app disconnected navigates (installed by
    /// `di::provide_controller`; unset in a process without the shell, where
    /// a close simply closes).
    static LANDING: RefCell<Option<Landing>> = const { RefCell::new(None) };
}

/// Install how [`ServersSwitcherController::close`] reaches Connect.
pub fn install_landing(go: Landing) {
    LANDING.with(|cell| *cell.borrow_mut() = Some(go));
}

/// A follow-on the paywall runs once Pro is unlocked.
pub type Retry = Rc<dyn Fn()>;

/// The two pieces of UI a switch can need, lent by the caller per
/// [`select`](ServersSwitcherController::select).
#[derive(Clone)]
pub struct SwitchGate {
    /// Raise the paywall; run the given retry once Pro is unlocked.
    pub paywall: Rc<dyn Fn(Retry)>,
    /// Open the add-server page prefilled with this endpoint.
    pub add_server: Rc<dyn Fn(String)>,
}

impl SwitchGate {
    /// The gate wired to the real paywall sheet over `overlays` and the
    /// add-server page behind `nav`.
    pub fn standard(
        overlays: &NavigatorController<AppState>,
        pro: &Arc<ProController>,
        nav: &Nav,
    ) -> Self {
        let overlays = overlays.clone();
        let pro = Arc::clone(pro);
        let nav = nav.clone();
        Self {
            paywall: Rc::new(move |retry| {
                show_upgrade_sheet(
                    &overlays,
                    Arc::clone(&pro),
                    UpgradeContext {
                        on_unlocked: retry,
                        server_limit: None,
                    },
                );
            }),
            add_server: Rc::new(move |endpoint| nav.push(&add_server_location(Some(&endpoint)))),
        }
    }
}

/// The endpoints of the component's peer cards, in peer order: every face but
/// the live one.
pub fn peer_endpoints(faces: &[ServerFace]) -> Vec<String> {
    faces
        .iter()
        .filter(|face| face.kind != FaceKind::Live)
        .map(|face| face.endpoint.clone())
        .collect()
}

/// What the Sessions page seeds itself from when a cached switch commits.
/// `generation` is unique per commit, so the page applies each seed once.
#[derive(Clone, Debug)]
pub struct ReconnectSeed {
    pub generation: u64,
    pub endpoint: String,
    pub host: String,
    pub page: Arc<PageSnapshot>,
}

/// The app bar banner's state for a background reconnect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconnectBanner {
    Connecting { host: String },
    Connected,
    Failed { host: String },
}

impl ReconnectBanner {
    /// The strip's text for this state.
    pub fn text(&self) -> String {
        match self {
            Self::Connecting { host } => format!("Connecting to {host}\u{2026}"),
            Self::Connected => "Connected".to_string(),
            Self::Failed { host } => format!("Couldn't reach {host}"),
        }
    }
}

/// The background dial in flight: its target and the seed generation that
/// started it. `dialed` is the connection's dial generation once it is known.
#[derive(Clone, Debug)]
struct BackgroundDial {
    endpoint: String,
    id: u64,
    dialed: Option<u64>,
}

/// The server switcher's view model. See the [module docs](self).
pub struct ServersSwitcherController {
    saved_servers: SavedServers,
    connection: Arc<ConnectionController>,
    pro: Arc<ProController>,
    keyring_unlock: KeyringUnlock,
    last_seen: LastSeenHandle,
    /// Whether the stage is up.
    pub open: RwSignal<bool>,
    /// The strip in its stable order (see the [module docs](self)).
    pub faces: RwSignal<Vec<ServerFace>>,
    /// The peer index growing into the live page, while a switch runs.
    pub expanding: RwSignal<Option<usize>>,
    /// The endpoint a switch is working on — its single-flight claim.
    pub busy: RwSignal<Option<String>>,
    /// Endpoint to the message of its last failed switch.
    pub failures: RwSignal<BTreeMap<String, String>>,
    /// Bumped by every [`close`](Self::close) and lock: a switch captures it
    /// and abandons silently when it moved while an unlock or confirm was
    /// pending, so a dismissed stage never dials.
    stage_epoch: AtomicU64,
    /// The connection's dial generation under which the in-flight claim's dial
    /// was started, once it is dialing (`None` while it is still unlocking or
    /// confirming). The connection drops a dial's `on_done` when a teardown or
    /// a newer dial supersedes it, so a claim whose generation moved can never
    /// be released by its dial: [`Self::release_if_stale`] does it.
    dial_claim: Mutex<Option<u64>>,
    /// The keyring's `lock_epoch` the last-seen map and recorded failures
    /// were last reconciled with; a move means a lock happened somewhere.
    seen_lock_epoch: AtomicU64,
    /// The strip's endpoint order for this process.
    strip_order: Mutex<Vec<String>>,
    /// The cached page the latest committed switch seeded (see the
    /// [module docs](self)); kept until the next commit or a lock.
    pub seed: RwSignal<Option<Arc<ReconnectSeed>>>,
    /// The app bar's connection banner, `None` when closed.
    pub banner: RwSignal<Option<ReconnectBanner>>,
    /// Bumped by every banner change: a clearing timer fires only if it still holds.
    banner_generation: AtomicU64,
    /// Source of [`ReconnectSeed::generation`].
    seed_generation: AtomicU64,
    /// The background dial in flight, if any.
    bg: Mutex<Option<BackgroundDial>>,
}

impl ServersSwitcherController {
    pub fn new(
        saved_servers: SavedServers,
        credentials: Credentials,
        connection: Arc<ConnectionController>,
        pro: Arc<ProController>,
        keyring: KeyringHandle,
        last_seen: LastSeenHandle,
    ) -> Self {
        let seen_lock_epoch = AtomicU64::new(keyring.lock_epoch());
        let keyring_unlock = KeyringUnlock::new(
            Arc::clone(&saved_servers),
            credentials,
            Arc::clone(&connection),
            keyring,
        );
        Self {
            saved_servers,
            connection,
            pro,
            keyring_unlock,
            last_seen,
            open: RwSignal::new(false),
            faces: RwSignal::new(Vec::new()),
            expanding: RwSignal::new(None),
            busy: RwSignal::new(None),
            failures: RwSignal::new(BTreeMap::new()),
            stage_epoch: AtomicU64::new(0),
            dial_claim: Mutex::new(None),
            seen_lock_epoch,
            strip_order: Mutex::new(Vec::new()),
            seed: RwSignal::new(None),
            banner: RwSignal::new(None),
            banner_generation: AtomicU64::new(0),
            seed_generation: AtomicU64::new(0),
            bg: Mutex::new(None),
        }
    }

    /// Re-read the strip from the saved list, the live connection, the
    /// keyring, the last-seen map and the recorded failures. A saved list that
    /// cannot be read leaves the previous strip in place.
    pub fn refresh_faces(&self) {
        self.release_if_stale();
        self.release_bg_if_stale();
        let Ok(saved) = self.saved_servers.list() else {
            return;
        };
        let active = self.active_endpoint();
        self.reconcile_memory(active.as_deref());
        let order = {
            let mut order = self.strip_order.lock().expect("strip order lock");
            let present: Vec<String> = saved.iter().map(|row| row.endpoint()).collect();
            order.retain(|e| present.contains(e));
            for endpoint in present {
                if !order.contains(&endpoint) {
                    order.push(endpoint);
                }
            }
            order.clone()
        };
        let faces = faces_for(
            &saved,
            &order,
            active.as_deref(),
            self.keyring_unlock.keyring().is_unlocked(),
            &self.last_seen.snapshot(),
            &self.failures.get_untracked(),
        );
        self.faces.set(faces);
    }

    /// Forget what a keyring lock (from any page) invalidated, and any failure
    /// of the endpoint now connected.
    fn reconcile_memory(&self, active: Option<&str>) {
        let lock_epoch = self.keyring_unlock.keyring().lock_epoch();
        if self.seen_lock_epoch.swap(lock_epoch, Ordering::SeqCst) != lock_epoch {
            self.last_seen.clear();
            self.failures.set(BTreeMap::new());
        }
        if let Some(endpoint) = active {
            let mut failures = self.failures.get_untracked();
            if failures.remove(endpoint).is_some() {
                self.failures.set(failures);
            }
        }
    }

    fn claim(&self) -> std::sync::MutexGuard<'_, Option<u64>> {
        self.dial_claim
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Give up the in-flight claim: `busy` and `expanding` together.
    fn release_claim(&self) {
        *self.claim() = None;
        self.busy.set(None);
        self.expanding.set(None);
    }

    /// Release a claim whose dial can no longer report: it dialed under a
    /// connection generation that a teardown or a newer dial has moved past.
    fn release_if_stale(&self) {
        if self.busy.get_untracked().is_none() {
            return;
        }
        let dialed = *self.claim();
        if dialed.is_some_and(|generation| generation != self.connection.dial_generation()) {
            self.release_claim();
        }
    }

    fn bg(&self) -> std::sync::MutexGuard<'_, Option<BackgroundDial>> {
        self.bg.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drop a background dial the connection can no longer report on (a
    /// teardown or a newer dial moved its generation), closing its banner.
    fn release_bg_if_stale(&self) {
        let stale = self.bg().as_ref().is_some_and(|bg| {
            bg.dialed
                .is_some_and(|generation| generation != self.connection.dial_generation())
        });
        if stale {
            *self.bg() = None;
            self.set_banner(None);
        }
    }

    /// Show `banner` (or close the strip), fencing every earlier timer.
    /// Answers the new banner generation.
    fn set_banner(&self, banner: Option<ReconnectBanner>) -> u64 {
        let generation = self.banner_generation.fetch_add(1, Ordering::SeqCst) + 1;
        if self.banner.get_untracked() != banner {
            self.banner.set(banner);
        }
        generation
    }

    /// Close the banner after `after_ms`, unless another change came first.
    fn clear_banner_later(self: &Arc<Self>, generation: u64, after_ms: u64) {
        let controller = Arc::clone(self);
        spawn_local(async move {
            clean_signals::time::sleep(std::time::Duration::from_millis(after_ms)).await;
            if controller.banner_generation.load(Ordering::SeqCst) == generation {
                controller.set_banner(None);
            }
        });
    }

    /// Show `Connecting` for `host` after the delay, unless the banner changed first.
    fn show_connecting_later(self: &Arc<Self>, generation: u64, host: String) {
        let controller = Arc::clone(self);
        spawn_local(async move {
            let delay = crate::features::servers::di::banner_connecting_delay_ms();
            clean_signals::time::sleep(std::time::Duration::from_millis(delay)).await;
            if controller.banner_generation.load(Ordering::SeqCst) == generation {
                controller.set_banner(Some(ReconnectBanner::Connecting { host }));
            }
        });
    }

    /// The host the Sessions page is still showing from a cached switch while
    /// the connection is down: what the bar's title reads in place of the
    /// disconnected name. Tracked.
    pub fn offline_host(&self) -> Option<String> {
        let seed = self.seed.get()?;
        (!self.connection.connected.get()).then(|| seed.host.clone())
    }

    /// The connected server's endpoint, if any; a background dial in flight
    /// counts as its target being live.
    fn active_endpoint(&self) -> Option<String> {
        if let Some(bg) = self.bg().as_ref() {
            return Some(bg.endpoint.clone());
        }
        if !self.connection.connected.get_untracked() {
            return None;
        }
        self.connection
            .server_info
            .get_untracked()
            .map(|info| info.endpoint)
    }

    /// Raise the stage over a fresh strip.
    pub fn open(&self) {
        self.refresh_faces();
        self.open.set(true);
    }

    /// Dismiss the stage; a half-grown card shrinks back with it. With no
    /// connection live or dialing (a failed switch tore the old one down) it
    /// lands on Connect, as the lock chip does, rather than leave a
    /// disconnected Sessions.
    pub fn close(&self) {
        self.close_with(|location| {
            if let Some(go) = LANDING.with(|cell| cell.borrow().clone()) {
                go(location);
            }
        });
    }

    /// [`Self::close`] with the navigation lent as a closure.
    pub fn close_with(&self, go: impl FnOnce(&str)) {
        let was_open = self.open.get_untracked();
        self.close_stage();
        if was_open
            && !self.connection.connected.get_untracked()
            && !self.connection.connecting.get_untracked()
        {
            go(LOCKED_LOCATION);
        }
    }

    /// Take the stage down without any navigation.
    pub(crate) fn close_stage(&self) {
        self.stage_epoch.fetch_add(1, Ordering::SeqCst);
        self.expanding.set(None);
        self.open.set(false);
    }

    /// Switch to the peer card at `peer_index` (resolved to its endpoint now,
    /// at the tap). Ignored while a switch is in flight.
    pub fn select(self: &Arc<Self>, peer_index: usize, gate: &SwitchGate) {
        self.release_if_stale();
        if self.busy.get_untracked().is_some() {
            return;
        }
        if let Some(endpoint) = self.endpoint_of_peer(peer_index) {
            self.select_endpoint(endpoint, gate);
        }
    }

    /// [`Self::select`] by endpoint — what the paywall's retry carries. Marks
    /// the card `expanding` at once; the rest runs on the UI executor (an
    /// unlock prompt may be pending). See the [module docs](self).
    pub fn select_endpoint(self: &Arc<Self>, endpoint: String, gate: &SwitchGate) {
        self.release_if_stale();
        if self.busy.get_untracked().is_some() {
            return;
        }
        // The stage this tap belongs to: the task abandons if it moved.
        let epoch = self.stage_epoch.load(Ordering::SeqCst);
        self.expanding.set(self.peer_index_of(&endpoint));
        let controller = Arc::clone(self);
        let gate = gate.clone();
        spawn_local(async move { controller.run_select_endpoint(endpoint, gate, epoch).await });
    }

    fn endpoint_of_peer(&self, peer_index: usize) -> Option<String> {
        peer_endpoints(&self.faces.get_untracked())
            .get(peer_index)
            .cloned()
    }

    fn peer_index_of(&self, endpoint: &str) -> Option<usize> {
        peer_endpoints(&self.faces.get_untracked())
            .iter()
            .position(|peer| peer == endpoint)
    }

    /// [`Self::select`]'s work for peer `peer_index`, awaitable.
    #[cfg(test)]
    pub async fn run_select(self: &Arc<Self>, peer_index: usize, gate: SwitchGate) {
        if let Some(endpoint) = self.endpoint_of_peer(peer_index) {
            let epoch = self.stage_epoch.load(Ordering::SeqCst);
            self.run_select_endpoint(endpoint, gate, epoch).await;
        }
    }

    /// The switch itself, awaitable. `epoch` is the stage epoch captured at
    /// the tap: a stage dismissed since abandons the switch.
    pub async fn run_select_endpoint(
        self: &Arc<Self>,
        endpoint: String,
        gate: SwitchGate,
        epoch: u64,
    ) {
        self.release_if_stale();
        if self.busy.get_untracked().is_some() || self.stage_epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        self.expanding.set(self.peer_index_of(&endpoint));
        let Some(row) = self.saved_row(&endpoint) else {
            self.expanding.set(None);
            self.refresh_faces();
            return;
        };
        if !row.has_credential {
            self.expanding.set(None);
            (gate.add_server)(endpoint);
            return;
        }
        let saved = self.saved_servers.list().unwrap_or_default();
        if let ServerLimitVerdict::Blocked { .. } = self.pro.verdict_for(&saved, &endpoint) {
            self.expanding.set(None);
            let controller = Arc::clone(self);
            let retry_gate = gate.clone();
            let retry_endpoint = endpoint.clone();
            // The retry belongs to the stage that raised the paywall.
            (gate.paywall)(Rc::new(move || {
                if controller.stage_epoch.load(Ordering::SeqCst) != epoch
                    || !controller.open.get_untracked()
                {
                    return;
                }
                controller.select_endpoint(retry_endpoint.clone(), &retry_gate);
            }));
            return;
        }
        if self.connection.connecting.get_untracked() && !self.supersede_bg() {
            self.expanding.set(None);
            return;
        }
        self.busy.set(Some(endpoint.clone()));
        if !self.keyring_unlock.keyring().is_unlocked() {
            let unlocked = self.unlock(&endpoint).await;
            if !unlocked {
                return;
            }
            if self.stage_epoch.load(Ordering::SeqCst) != epoch {
                self.abandon();
                return;
            }
        }
        self.vet_and_dial(endpoint, gate, epoch);
    }

    /// Decide everything on a **fresh** row (the unlock may have taken a
    /// while): gone -> abandon; no credential -> the add page; another
    /// connect underway -> abandon; else dial (a saved trust-anyway row too).
    /// `busy` is held by the caller.
    fn vet_and_dial(self: &Arc<Self>, endpoint: String, gate: SwitchGate, epoch: u64) {
        let Some(row) = self.saved_row(&endpoint) else {
            self.abandon();
            return;
        };
        if !row.has_credential {
            self.abandon();
            (gate.add_server)(endpoint);
            return;
        }
        if self.connection.connecting.get_untracked() && !self.supersede_bg() {
            self.abandon();
            return;
        }
        if let Some(page) = self.cached_page(&endpoint)
            && self.stage_epoch.load(Ordering::SeqCst) == epoch
        {
            self.commit_cached(&endpoint, page);
            return;
        }
        self.dial(&endpoint, epoch);
    }

    /// Cancel the background dial in flight, if there is one: the connection is
    /// torn down (its dial's outcome is dropped) so the next switch can dial.
    /// `false` when the dial in flight is not a background one.
    fn supersede_bg(&self) -> bool {
        if self.bg().take().is_none() {
            return false;
        }
        self.connection.disconnect();
        self.set_banner(None);
        true
    }

    /// The page `endpoint` last showed, if the last-seen map filed one.
    fn cached_page(&self, endpoint: &str) -> Option<Arc<PageSnapshot>> {
        self.last_seen.snapshot().get(endpoint)?.page.clone()
    }

    /// The cached switch: seed the page, close the stage at once (no peers) and
    /// dial in the background. `busy` is held by the caller and released here.
    fn commit_cached(self: &Arc<Self>, endpoint: &str, page: Arc<PageSnapshot>) {
        let found = self
            .saved_row(endpoint)
            .filter(|row| row.has_credential)
            .and_then(|row| Some((self.keyring_unlock.keyring().token_for(endpoint)?, row)));
        let epoch = self.stage_epoch.load(Ordering::SeqCst);
        let Some((token, row)) = found else {
            self.fail(endpoint, epoch, NO_CREDENTIAL_MESSAGE.to_string());
            return;
        };
        let id = self.seed_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let host = row.host.clone();
        *self.bg() = Some(BackgroundDial {
            endpoint: endpoint.to_string(),
            id,
            dialed: None,
        });
        // `Connecting` shows only if the dial is still pending after the delay;
        // any later banner change (landing, failure, a newer switch) fences it.
        let pending = self.set_banner(None);
        self.show_connecting_later(pending, host.clone());
        self.seed.set(Some(Arc::new(ReconnectSeed {
            generation: id,
            endpoint: endpoint.to_string(),
            host,
            page,
        })));
        self.release_claim();
        // Faces first, so the target is the live card; the close then builds
        // no peers (freeze fix v3).
        self.refresh_faces();
        crate::features::servers::di::skip_close_tail();
        self.close_stage();
        let controller = Arc::clone(self);
        let dialed = endpoint.to_string();
        self.keyring_unlock
            .switch_to_saved(&row, token, move |outcome| match outcome {
                Ok(()) => controller.settle_bg(&dialed, id),
                Err(failure) => {
                    use clean_signals::Failure;
                    controller.fail_bg(&dialed, id, failure.user_message());
                }
            });
        if let Some(bg) = self.bg().as_mut()
            && bg.id == id
        {
            bg.dialed = Some(self.connection.dial_generation());
        }
    }

    /// Takes background dial `id` if it is still the one in flight.
    fn finish_bg(&self, id: u64) -> bool {
        let mut bg = self.bg();
        if bg.as_ref().is_some_and(|dial| dial.id == id) {
            *bg = None;
            return true;
        }
        false
    }

    /// The background dial landed: flash `Connected`, then clear.
    fn settle_bg(self: &Arc<Self>, endpoint: &str, id: u64) {
        if !self.finish_bg(id) {
            return;
        }
        let mut failures = self.failures.get_untracked();
        failures.remove(endpoint);
        self.failures.set(failures);
        self.refresh_faces();
        // Flash only if `Connecting` was actually shown; else stay silent.
        let shown = matches!(
            self.banner.get_untracked(),
            Some(ReconnectBanner::Connecting { .. })
        );
        if !shown {
            self.set_banner(None);
            return;
        }
        let generation = self.set_banner(Some(ReconnectBanner::Connected));
        self.clear_banner_later(
            generation,
            crate::features::servers::di::banner_success_ms(),
        );
    }

    /// The background dial failed: the cached page stays, the face is marked,
    /// the banner reads `Failed` until the next change or its timeout.
    fn fail_bg(self: &Arc<Self>, endpoint: &str, id: u64, message: String) {
        if !self.finish_bg(id) {
            return;
        }
        let mut failures = self.failures.get_untracked();
        failures.insert(endpoint.to_string(), message);
        self.failures.set(failures);
        self.refresh_faces();
        let host = self
            .seed
            .get_untracked()
            .map(|seed| seed.host.clone())
            .unwrap_or_else(|| endpoint.to_string());
        let generation = self.set_banner(Some(ReconnectBanner::Failed { host }));
        self.clear_banner_later(
            generation,
            crate::features::servers::di::banner_failure_ms(),
        );
    }

    /// The saved row for `endpoint`, read fresh.
    fn saved_row(&self, endpoint: &str) -> Option<SavedServer> {
        self.saved_servers
            .list()
            .ok()?
            .into_iter()
            .find(|row| row.endpoint() == endpoint)
    }

    /// Dial `endpoint` with the keyring's token, the row read fresh and only
    /// while it still claims a credential the keyring holds. `busy` is held.
    fn dial(self: &Arc<Self>, endpoint: &str, epoch: u64) {
        let found = self
            .saved_row(endpoint)
            .filter(|row| row.has_credential)
            .and_then(|row| Some((self.keyring_unlock.keyring().token_for(endpoint)?, row)));
        let Some((token, row)) = found else {
            self.fail(endpoint, epoch, NO_CREDENTIAL_MESSAGE.to_string());
            return;
        };
        let controller = Arc::clone(self);
        let dialed = endpoint.to_string();
        self.keyring_unlock
            .switch_to_saved(&row, token, move |outcome| match outcome {
                Ok(()) => controller.settle(&dialed, epoch),
                Err(failure) => {
                    use clean_signals::Failure;
                    controller.fail(&dialed, epoch, failure.user_message());
                }
            });
        // A dial that settled in place already released the claim; one still
        // pending is tied to the generation it started under.
        if self.busy.get_untracked().is_some() {
            *self.claim() = Some(self.connection.dial_generation());
        }
    }

    /// One `unlock_all` prompt; `false` (state already reset) when the dial
    /// must not go on. A cancel or a superseding lock is silent.
    async fn unlock(&self, endpoint: &str) -> bool {
        match self.keyring_unlock.unlock().await {
            KeyringUnlockOutcome::Unlocked { .. } => true,
            KeyringUnlockOutcome::Refused(status) => {
                if let Some(message) = status.message() {
                    app_toasts().show(message);
                }
                self.abandon();
                false
            }
            KeyringUnlockOutcome::ListUnavailable => {
                app_toasts().show(
                    crate::features::connect::presentation::controller::SAVED_LIST_UNAVAILABLE_MESSAGE,
                );
                self.abandon();
                false
            }
            KeyringUnlockOutcome::Busy | KeyringUnlockOutcome::Superseded => {
                log::debug!("servers: unlock for {endpoint} abandoned silently");
                self.abandon();
                false
            }
        }
    }

    /// Give up a switch with nothing marked failed.
    fn abandon(&self) {
        self.release_claim();
        self.refresh_faces();
    }

    /// The connection settled on `endpoint`: stage down, card grown, together
    /// (both writes are synchronous, so no frame sits between them). A dial
    /// from a stage since dismissed only releases its claim: the stage a
    /// later open raised is not its to take down.
    fn settle(&self, endpoint: &str, epoch: u64) {
        let mut failures = self.failures.get_untracked();
        failures.remove(endpoint);
        self.failures.set(failures);
        let current = self.stage_epoch.load(Ordering::SeqCst) == epoch;
        *self.claim() = None;
        self.busy.set(None);
        if current {
            // Faces first, so `live_index` names the new live server; the close
            // then builds no peers (a settle has no tail), so nothing can jump.
            self.refresh_faces();
            crate::features::servers::di::skip_close_tail();
            self.expanding.set(None);
            self.open.set(false);
        } else {
            self.refresh_faces();
        }
    }

    /// The dial to `endpoint` failed: reverse the ramp, mark the face. A dial
    /// from a stage since dismissed leaves the current stage's ramp alone.
    fn fail(&self, endpoint: &str, epoch: u64, message: String) {
        let mut failures = self.failures.get_untracked();
        failures.insert(endpoint.to_string(), message);
        self.failures.set(failures);
        *self.claim() = None;
        self.busy.set(None);
        if self.stage_epoch.load(Ordering::SeqCst) == epoch {
            self.expanding.set(None);
        }
        self.refresh_faces();
    }

    /// Lock the keyring, drop the connection and return to Connect.
    pub fn lock(&self, nav: &Nav) {
        self.lock_with(|location| nav.go(location));
    }

    /// [`Self::lock`] with the navigation lent as a closure.
    pub fn lock_with(&self, go: impl FnOnce(&str)) {
        self.stage_epoch.fetch_add(1, Ordering::SeqCst);
        self.failures.set(BTreeMap::new());
        self.last_seen.clear();
        *self.bg() = None;
        self.set_banner(None);
        self.seed.set(None);
        self.connection
            .lock_and_disconnect(self.keyring_unlock.keyring());
        self.release_claim();
        self.close_stage();
        go(LOCKED_LOCATION);
    }

    /// Open the add-server page, mirroring Connect's QR scan on the free
    /// tier: a blocked add raises the paywall in place and leaves the stage
    /// open; an allowed (or just-unlocked) add closes the stage and pushes.
    pub fn add(self: &Arc<Self>, gate: &SwitchGate, nav: &Nav) {
        let nav = nav.clone();
        self.add_with(&gate.paywall, Rc::new(move |location| nav.push(location)));
    }

    /// [`Self::add`] with the paywall and the navigation lent as closures.
    pub fn add_with(self: &Arc<Self>, paywall: &Rc<dyn Fn(Retry)>, push: Rc<dyn Fn(&str)>) {
        let saved = self.saved_servers.list().unwrap_or_default();
        if let ServerLimitVerdict::Blocked { .. } = self.pro.new_server_verdict(&saved) {
            let epoch = self.stage_epoch.load(Ordering::SeqCst);
            let controller = Arc::clone(self);
            // The retry belongs to the stage that raised the paywall.
            paywall(Rc::new(move || {
                if controller.stage_epoch.load(Ordering::SeqCst) != epoch
                    || !controller.open.get_untracked()
                {
                    return;
                }
                controller.open_add_page(&*push);
            }));
            return;
        }
        self.open_add_page(&*push);
    }

    fn open_add_page(&self, push: &dyn Fn(&str)) {
        // The stage must not outlive the push (its open flag, poll pause and
        // strip would persist under the add page and reappear on pop).
        self.close_stage();
        push(ADD_SERVER_LOCATION);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::Mutex;

    use super::*;
    use crate::connection::{
        ConnectRun, ConnectTarget, ConnectionBackend, MockConnectionBackend, TrustMode,
    };
    use crate::failure::AppFailure;
    use crate::features::connect::data::repositories::{
        MockCredentialService, MockSavedServersRepository,
    };
    use crate::features::connect::domain::entities::SavedServer;
    use crate::features::pro::data::repositories::MockEntitlementRepository;
    use crate::features::servers::domain::entities::SessionGlimpse;

    /// A backend whose every dial fails, until told to succeed.
    struct FlakyBackend {
        fail: Mutex<bool>,
    }

    impl ConnectionBackend for FlakyBackend {
        fn begin_connect(&self, target: ConnectTarget) -> ConnectRun {
            if *self.fail.lock().unwrap() {
                return ConnectRun::Ready(Err(AppFailure::Network("refused".to_string())));
            }
            MockConnectionBackend.begin_connect(target)
        }

        fn disconnect(&self) {}
    }

    fn server(host: &str, last_used_ms: i64, has_credential: bool) -> SavedServer {
        SavedServer {
            host: host.to_string(),
            port: 7777,
            trust_mode: TrustMode::System,
            label: host.to_string(),
            last_used_ms,
            read_only: false,
            has_credential,
            token_name: None,
            fingerprint: None,
        }
    }

    struct Rig {
        controller: Arc<ServersSwitcherController>,
        connection: Arc<ConnectionController>,
        credentials: Arc<MockCredentialService>,
        keyring: KeyringHandle,
        last_seen: LastSeenHandle,
        saved: SavedServers,
    }

    fn rig_with(backend: Arc<dyn ConnectionBackend>, rows: Vec<SavedServer>, pro: bool) -> Rig {
        rig_over(backend, rows, pro, |mock| mock as Credentials)
    }

    /// A credential store whose `unlock_all` waits for the test to release it,
    /// so a prompt can be pending while the stage is dismissed.
    struct GatedCredentials {
        inner: Arc<MockCredentialService>,
        release: Arc<tokio::sync::Notify>,
    }

    #[clean_signals::async_trait]
    impl crate::features::connect::domain::repositories::CredentialService for GatedCredentials {
        async fn store(
            &self,
            request: crate::features::connect::domain::entities::StoreRequest,
        ) -> crate::features::connect::domain::entities::CredentialStatus {
            self.inner.store(request).await
        }

        async fn unlock(
            &self,
            request: crate::features::connect::domain::entities::UnlockRequest,
        ) -> crate::features::connect::domain::entities::UnlockedCredential {
            self.inner.unlock(request).await
        }

        async fn unlock_all(
            &self,
            reason: &str,
            endpoints: &[String],
        ) -> crate::features::connect::domain::entities::UnlockedKeyring {
            self.release.notified().await;
            self.inner.unlock_all(reason, endpoints).await
        }

        async fn store_in_vault(
            &self,
            request: crate::features::connect::domain::entities::StoreRequest,
            keyring: &mut crate::features::connect::domain::keyring::Keyring,
        ) -> crate::features::connect::domain::entities::CredentialStatus {
            self.inner.store_in_vault(request, keyring).await
        }

        async fn delete_from_vault(
            &self,
            server_key: String,
            keyring: &mut crate::features::connect::domain::keyring::Keyring,
        ) -> Result<(), AppFailure> {
            self.inner.delete_from_vault(server_key, keyring).await
        }

        async fn delete(&self, server_key: String) -> Result<(), AppFailure> {
            self.inner.delete(server_key).await
        }

        async fn reset_vault(&self) -> Result<(), AppFailure> {
            self.inner.reset_vault().await
        }
    }

    fn rig_over(
        backend: Arc<dyn ConnectionBackend>,
        rows: Vec<SavedServer>,
        pro: bool,
        over: impl FnOnce(Arc<MockCredentialService>) -> Credentials,
    ) -> Rig {
        let credentials = Arc::new(MockCredentialService::new());
        let connection = Arc::new(ConnectionController::with_backend(backend));
        let entitlement = Arc::new(if pro {
            MockEntitlementRepository::pro()
        } else {
            MockEntitlementRepository::free()
        });
        let keyring = KeyringHandle::new();
        let last_seen = LastSeenHandle::new();
        let saved: SavedServers = Arc::new(MockSavedServersRepository::with_servers(rows));
        let controller = Arc::new(ServersSwitcherController::new(
            Arc::clone(&saved),
            over(Arc::clone(&credentials)),
            Arc::clone(&connection),
            Arc::new(ProController::new(entitlement)),
            keyring.clone(),
            last_seen.clone(),
        ));
        Rig {
            controller,
            connection,
            credentials,
            keyring,
            last_seen,
            saved,
        }
    }

    fn rig(rows: Vec<SavedServer>, pro: bool) -> Rig {
        rig_with(Arc::new(MockConnectionBackend), rows, pro)
    }

    /// Connect to `host` first, so the strip has a live card.
    fn connect_to(rig: &Rig, host: &str) {
        rig.connection.connect(ConnectTarget {
            host: host.to_string(),
            port: 7777,
            trust_mode: TrustMode::NoVerification,
            label: host.to_string(),
            offer_credential: false,
            token: zeroize::Zeroizing::new("t".to_string()),
            remember: false,
            fingerprint: None,
        });
    }

    fn unlocked(rig: &Rig, entries: &[(&str, &str)]) {
        let mut keyring = crate::features::connect::domain::keyring::Keyring::new();
        for (endpoint, token) in entries {
            keyring.insert(*endpoint, zeroize::Zeroizing::new((*token).to_string()));
        }
        rig.keyring.publish(keyring);
    }

    /// A gate recording what it was asked, raising nothing.
    struct Probe {
        gate: SwitchGate,
        paywalls: Rc<Cell<usize>>,
        added: Rc<RefCell<Vec<String>>>,
    }

    fn probe() -> Probe {
        let paywalls = Rc::new(Cell::new(0));
        let added = Rc::new(RefCell::new(Vec::new()));
        let gate = SwitchGate {
            paywall: {
                let paywalls = Rc::clone(&paywalls);
                Rc::new(move |_retry| paywalls.set(paywalls.get() + 1))
            },
            add_server: {
                let added = Rc::clone(&added);
                Rc::new(move |endpoint| added.borrow_mut().push(endpoint))
            },
        };
        Probe {
            gate,
            paywalls,
            added,
        }
    }

    #[tokio::test]
    async fn select_on_pro_switches_and_settles_open_and_expanding() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        assert!(rig.controller.open.get_untracked());
        let epoch = rig.connection.connection_epoch.get_untracked();
        assert_eq!(
            peer_endpoints(&rig.controller.faces.get_untracked()),
            ["b:7777"]
        );

        rig.controller.run_select(0, probe().gate).await;

        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert!(rig.connection.connection_epoch.get_untracked() > epoch);
        assert!(!rig.controller.open.get_untracked());
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert_eq!(rig.controller.busy.get_untracked(), None);
    }

    #[tokio::test]
    async fn select_on_a_free_install_with_two_rows_raises_the_paywall_and_never_dials() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], false);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        let epoch = rig.connection.connection_epoch.get_untracked();
        let probe = probe();

        rig.controller.run_select(0, probe.gate.clone()).await;

        assert_eq!(probe.paywalls.get(), 1);
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert!(rig.controller.open.get_untracked(), "the stage stays up");
        assert_eq!(rig.connection.connection_epoch.get_untracked(), epoch);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "a:7777");
    }

    #[tokio::test]
    async fn a_locked_face_prompts_once_then_switches() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        rig.credentials
            .seed_vault(&[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        assert_eq!(
            rig.controller.faces.get_untracked()[1].kind,
            FaceKind::Locked
        );

        rig.controller.run_select(0, probe().gate).await;

        assert_eq!(rig.credentials.prompts().len(), 1);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert!(rig.keyring.is_unlocked());
        assert!(!rig.controller.open.get_untracked());
    }

    #[tokio::test]
    async fn a_failed_dial_marks_the_face_failed_and_a_second_tap_retries() {
        let backend = Arc::new(FlakyBackend {
            fail: Mutex::new(false),
        });
        let rig = rig_with(
            Arc::clone(&backend) as Arc<dyn ConnectionBackend>,
            vec![server("a", 20, true), server("b", 10, true)],
            true,
        );
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        *backend.fail.lock().unwrap() = true;

        rig.controller.run_select(0, probe().gate).await;

        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert!(
            rig.controller.open.get_untracked(),
            "the stage stays open on failure"
        );
        assert!(matches!(
            rig.controller.faces.get_untracked().last().map(|f| &f.kind),
            Some(FaceKind::Failed { .. })
        ));

        // `switch_to` tears the old connection down before it dials, so after
        // the failure no face is live and the strip's peers are every row.
        assert!(!rig.connection.connected.get_untracked());
        let peers = peer_endpoints(&rig.controller.faces.get_untracked());
        let again = peers
            .iter()
            .position(|e| e == "b:7777")
            .expect("b is a peer");

        *backend.fail.lock().unwrap() = false;
        rig.controller.run_select(again, probe().gate).await;

        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert!(!rig.controller.open.get_untracked());
        assert!(rig.controller.failures.get_untracked().is_empty());
    }

    #[tokio::test]
    async fn a_row_without_a_credential_goes_to_the_add_page_not_the_keyring() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, false)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta")]);
        rig.controller.open();
        let probe = probe();

        rig.controller.run_select(0, probe.gate.clone()).await;

        assert_eq!(*probe.added.borrow(), ["b:7777"]);
        assert_eq!(rig.credentials.prompts().len(), 0);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "a:7777");
        assert_eq!(rig.controller.expanding.get_untracked(), None);
    }

    #[test]
    fn close_clears_expanding_and_lock_drops_the_connection() {
        let rig = rig(vec![server("a", 20, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta")]);
        rig.controller.open();
        rig.controller.expanding.set(Some(0));
        rig.controller.close();
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert!(!rig.controller.open.get_untracked());

        let went = RefCell::new(None);
        rig.controller
            .lock_with(|to| *went.borrow_mut() = Some(to.to_string()));
        assert_eq!(went.borrow().as_deref(), Some("/"));
        assert!(!rig.keyring.is_unlocked());
        assert!(!rig.connection.connected.get_untracked());

        let pushed = Rc::new(RefCell::new(None));
        let paywall: Rc<dyn Fn(Retry)> = Rc::new(|_| panic!("a Pro add never meets the paywall"));
        let sink = Rc::clone(&pushed);
        rig.controller.add_with(
            &paywall,
            Rc::new(move |to| *sink.borrow_mut() = Some(to.to_string())),
        );
        assert_eq!(pushed.borrow().as_deref(), Some("/?mode=add"));
    }

    #[test]
    fn a_cached_face_reads_the_last_seen_handle() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.last_seen.record_at(
            "b:7777",
            vec![SessionGlimpse {
                name: "main".to_string(),
                backend: "zellij".to_string(),
                running: true,
                meta_line: "1h".to_string(),
            }],
            7,
        );
        rig.controller.open();
        assert!(matches!(
            rig.controller.faces.get_untracked()[1].kind,
            FaceKind::Cached {
                last_seen_ms: 7,
                ..
            }
        ));
    }

    /// A `NoVerification` credentialed peer `b` under a live `a`, keyring open.
    fn unverified_rig() -> Rig {
        let mut b = server("b", 10, true);
        b.trust_mode = TrustMode::NoVerification;
        let rig = rig(vec![server("a", 20, true), b], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        rig
    }

    #[tokio::test]
    async fn a_saved_unverified_row_dials_straight_through() {
        let rig = unverified_rig();
        let probe = probe();

        rig.controller.run_select(0, probe.gate.clone()).await;

        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert!(!rig.controller.open.get_untracked());
        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert_eq!(probe.paywalls.get(), 0);
        assert!(probe.added.borrow().is_empty());
    }

    #[tokio::test]
    async fn closing_the_stage_while_the_unlock_is_pending_dials_nothing() {
        let release = Arc::new(tokio::sync::Notify::new());
        let gate_release = Arc::clone(&release);
        let rig = rig_over(
            Arc::new(MockConnectionBackend),
            vec![server("a", 20, true), server("b", 10, true)],
            true,
            move |inner| {
                inner.seed_vault(&[("a:7777", "ta"), ("b:7777", "tb")]);
                Arc::new(GatedCredentials {
                    inner,
                    release: gate_release,
                }) as Credentials
            },
        );
        connect_to(&rig, "a");
        rig.controller.open();
        let epoch = rig.connection.connection_epoch.get_untracked();
        let probe = probe();

        let select = rig.controller.run_select(0, probe.gate.clone());
        let dismiss = async {
            tokio::task::yield_now().await;
            rig.controller.close();
            release.notify_one();
        };
        tokio::join!(select, dismiss);

        assert_eq!(rig.connection.connection_epoch.get_untracked(), epoch);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "a:7777");
        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert_eq!(rig.controller.expanding.get_untracked(), None);
    }

    #[test]
    fn a_tap_during_an_in_flight_switch_leaves_expanding_alone() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        rig.controller.open();
        rig.controller.busy.set(Some("b:7777".to_string()));
        rig.controller.expanding.set(Some(3));

        rig.controller.select(0, &probe().gate);

        assert_eq!(rig.controller.expanding.get_untracked(), Some(3));
    }

    #[test]
    fn lock_forgets_recorded_failures() {
        let rig = rig(vec![server("a", 20, true)], true);
        rig.controller
            .failures
            .set(BTreeMap::from([("a:7777".to_string(), "down".to_string())]));

        rig.controller.lock_with(|_| {});

        assert!(rig.controller.failures.get_untracked().is_empty());
    }

    /// A locked rig over `a` (live) and `b`, whose `unlock_all` waits on the
    /// returned notify.
    fn gated_rig(rows: Vec<SavedServer>) -> (Rig, Arc<tokio::sync::Notify>) {
        let release = Arc::new(tokio::sync::Notify::new());
        let gate_release = Arc::clone(&release);
        let rig = rig_over(Arc::new(MockConnectionBackend), rows, true, move |inner| {
            inner.seed_vault(&[("a:7777", "ta"), ("b:7777", "tb")]);
            Arc::new(GatedCredentials {
                inner,
                release: gate_release,
            }) as Credentials
        });
        connect_to(&rig, "a");
        rig.controller.open();
        (rig, release)
    }

    /// Pretend a dial started under the connection's current generation is
    /// still pending for `b`.
    fn claim_pending_dial(rig: &Rig) {
        rig.controller.busy.set(Some("b:7777".to_string()));
        *rig.controller.claim() = Some(rig.connection.dial_generation());
    }

    #[test]
    fn a_claim_whose_dial_was_superseded_is_released_at_the_next_tap() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        claim_pending_dial(&rig);
        // The dial is still genuinely pending: taps are swallowed.
        rig.controller.select(0, &probe().gate);
        assert!(rig.controller.busy.get_untracked().is_some());

        // A teardown drops the dial's `on_done`; the connection is then
        // re-established elsewhere.
        rig.connection.disconnect();
        connect_to(&rig, "a");
        rig.controller.expanding.set(Some(0));
        rig.controller.open();

        assert_eq!(rig.controller.busy.get_untracked(), None, "released");
        assert_eq!(rig.controller.expanding.get_untracked(), None);
    }

    #[tokio::test]
    async fn a_tap_after_a_superseded_dial_switches_instead_of_being_swallowed() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        claim_pending_dial(&rig);
        rig.connection.disconnect();
        connect_to(&rig, "a");
        rig.controller.open();

        rig.controller.run_select(0, probe().gate).await;

        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert_eq!(rig.controller.busy.get_untracked(), None);
    }

    #[tokio::test]
    async fn a_connect_started_during_the_unlock_prompt_leaves_nothing_busy() {
        let (rig, release) = gated_rig(vec![server("a", 20, true), server("b", 10, true)]);
        let epoch = rig.connection.connection_epoch.get_untracked();
        let probe = probe();

        let select = rig.controller.run_select(0, probe.gate.clone());
        let meanwhile = async {
            tokio::task::yield_now().await;
            rig.connection.connecting.set(true);
            release.notify_one();
        };
        tokio::join!(select, meanwhile);

        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert_eq!(rig.connection.connection_epoch.get_untracked(), epoch);
    }

    #[test]
    fn a_late_settle_or_fail_of_a_dismissed_stage_leaves_a_reopened_stage_alone() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        rig.controller.open();
        let dialed_under = rig.controller.stage_epoch.load(Ordering::SeqCst);
        rig.controller.close_stage();
        rig.controller.open();
        rig.controller.expanding.set(Some(0));
        rig.controller.busy.set(Some("b:7777".to_string()));

        rig.controller.settle("b:7777", dialed_under);

        assert!(rig.controller.open.get_untracked(), "still up");
        assert_eq!(rig.controller.expanding.get_untracked(), Some(0));
        assert_eq!(rig.controller.busy.get_untracked(), None, "claim freed");

        rig.controller.busy.set(Some("b:7777".to_string()));
        rig.controller
            .fail("b:7777", dialed_under, "down".to_string());

        assert!(rig.controller.open.get_untracked());
        assert_eq!(rig.controller.expanding.get_untracked(), Some(0));
        assert_eq!(rig.controller.busy.get_untracked(), None);
    }

    #[tokio::test]
    async fn a_switch_whose_stage_was_dismissed_before_the_task_ran_dials_nothing() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        let tapped_under = rig.controller.stage_epoch.load(Ordering::SeqCst);
        let connection_epoch = rig.connection.connection_epoch.get_untracked();
        rig.controller.close_stage();

        rig.controller
            .run_select_endpoint("b:7777".to_string(), probe().gate, tapped_under)
            .await;

        assert_eq!(
            rig.connection.connection_epoch.get_untracked(),
            connection_epoch
        );
        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert_eq!(rig.controller.expanding.get_untracked(), None);
    }

    #[tokio::test]
    async fn a_paywall_retry_from_a_dismissed_or_closed_stage_does_nothing() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], false);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        let retries: Rc<RefCell<Vec<Retry>>> = Rc::new(RefCell::new(Vec::new()));
        let mut gate = probe().gate;
        gate.paywall = {
            let retries = Rc::clone(&retries);
            Rc::new(move |retry| retries.borrow_mut().push(retry))
        };
        rig.controller.run_select(0, gate.clone()).await;
        assert_eq!(retries.borrow().len(), 1);

        rig.controller.close_stage();
        rig.controller.open();
        retries.borrow()[0]();
        assert_eq!(
            rig.controller.expanding.get_untracked(),
            None,
            "a retry from another stage never starts a switch"
        );

        rig.controller.run_select(0, gate).await;
        rig.controller.close_stage();
        retries.borrow()[1]();
        assert_eq!(rig.controller.expanding.get_untracked(), None);
    }

    #[tokio::test]
    async fn a_credential_forgotten_during_the_unlock_goes_to_the_add_page() {
        let (rig, release) = gated_rig(vec![server("a", 20, true), server("b", 10, true)]);
        let probe = probe();

        let select = rig.controller.run_select(0, probe.gate.clone());
        let meanwhile = async {
            tokio::task::yield_now().await;
            rig.saved.save(server("b", 10, false)).unwrap();
            release.notify_one();
        };
        tokio::join!(select, meanwhile);

        assert_eq!(*probe.added.borrow(), ["b:7777"]);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "a:7777", "nothing was dialed");
        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert!(rig.controller.failures.get_untracked().is_empty());
    }

    #[tokio::test]
    async fn a_row_turning_unverified_during_the_unlock_still_dials() {
        let (rig, release) = gated_rig(vec![server("a", 20, true), server("b", 10, true)]);
        let probe = probe();

        let select = rig.controller.run_select(0, probe.gate.clone());
        let meanwhile = async {
            tokio::task::yield_now().await;
            let mut b = server("b", 10, true);
            b.trust_mode = TrustMode::NoVerification;
            rig.saved.save(b).unwrap();
            release.notify_one();
        };
        tokio::join!(select, meanwhile);

        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
    }

    #[test]
    fn any_keyring_lock_drops_last_seen_and_failures_at_the_next_strip() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.last_seen.record_at("b:7777", Vec::new(), 7);
        rig.controller
            .failures
            .set(BTreeMap::from([("b:7777".to_string(), "down".to_string())]));

        // A lock from Settings or the Sessions disconnect: not the chip.
        rig.keyring.lock();
        rig.controller.open();

        assert!(rig.last_seen.snapshot().is_empty());
        assert!(rig.controller.failures.get_untracked().is_empty());
    }

    #[test]
    fn a_failure_is_dropped_once_that_endpoint_connects_by_any_path() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        rig.controller.failures.set(BTreeMap::from([
            ("a:7777".to_string(), "down".to_string()),
            ("b:7777".to_string(), "down".to_string()),
        ]));
        connect_to(&rig, "a");

        rig.controller.open();

        let failures = rig.controller.failures.get_untracked();
        assert!(!failures.contains_key("a:7777"));
        assert!(failures.contains_key("b:7777"), "other faces keep theirs");
    }

    #[test]
    fn closing_with_no_connection_live_or_dialing_lands_on_connect() {
        let rig = rig(vec![server("a", 20, true)], true);
        rig.controller.open();
        let went = RefCell::new(None);
        rig.controller
            .close_with(|to| *went.borrow_mut() = Some(to.to_string()));
        assert_eq!(went.borrow().as_deref(), Some("/"));

        connect_to(&rig, "a");
        rig.controller.open();
        let again = RefCell::new(None);
        rig.controller
            .close_with(|to| *again.borrow_mut() = Some(to.to_string()));
        assert_eq!(*again.borrow(), None, "a live connection stays put");

        rig.connection.disconnect();
        rig.connection.connecting.set(true);
        rig.controller.open();
        let dialing = RefCell::new(None);
        rig.controller
            .close_with(|to| *dialing.borrow_mut() = Some(to.to_string()));
        assert_eq!(*dialing.borrow(), None, "a dial in flight stays put");
    }

    #[test]
    fn add_closes_the_stage_itself() {
        let rig = rig(vec![server("a", 20, true)], true);
        connect_to(&rig, "a");
        rig.controller.open();
        let paywall: Rc<dyn Fn(Retry)> = Rc::new(|_| panic!("a Pro add never meets the paywall"));
        rig.controller.add_with(&paywall, Rc::new(|_| {}));
        assert!(!rig.controller.open.get_untracked());
    }
}
