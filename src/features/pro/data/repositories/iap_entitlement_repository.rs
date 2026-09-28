//! [`IapEntitlementRepository`] — the real [`EntitlementRepository`], over
//! `frust-iap`'s Play Billing/StoreKit surface and an [`EntitlementCache`].
//!
//! **A purchase is request → event → grant → settle.** `request_purchase` only
//! acknowledges that the store took the request; the registered listener is the
//! single source of truth for what actually happened. An `Err` from the request
//! is therefore classified rather than obeyed — only a timed-out dispatch ack
//! leaves a flow that may still be running, and only that one waits for an
//! event (`classify_request` holds the whole table). Settlement is last: Play
//! auto-refunds an unacknowledged purchase after ~3 days, so every Purchased row
//! this sees — from a live event or from the entitlement query at launch — is
//! finished as soon as the grant is durable, and a failed settle never revokes
//! the grant, since the store re-delivers the row.
//!
//! **The listener is registered once, in [`IapEntitlementRepository::new`], and
//! its handle lives as long as this repository** (dropping it unregisters). The
//! callback does nothing but hand the event to a channel: it runs on the
//! plugin's one event-delivery thread, where blocking costs every later event
//! and any store call is refused outright.
//!
//! **Errors are logged by code, never by payload.** A store message can quote
//! the document it choked on, which for a purchase embeds the bearer token, so
//! nothing here formats an `IapError`/`IapPurchaseError` with `Display` — see
//! `store_code` and `purchase_message`.
//!
//! **Desktop has no store at all.** The first `NotAvailable` answer latches a
//! flag, so the desktop preview asks once, logs one line, and afterwards reads
//! straight from the cache instead of retrying on every gate.
//!
//! `StoreApi` is the seam the whole state machine above is tested through:
//! `PlayStore` delegates each of its seven members to `frust_iap::Iap`
//! one-to-one, and the `cfg(test)` fake answers from scripted rows.

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[cfg(not(test))]
use frust::spawn_blocking;
use frust_iap::{
    FetchProductsResult, Iap, IapError, IapErrorCode, IapEvent, IapPurchaseError, ListenerHandle,
    Product, ProductQueryType, ProductRequest, Purchase, PurchaseInput, PurchaseOptions,
    PurchaseState, RequestPurchaseProps, RequestPurchasePropsByPlatforms,
};
use frust_shared_preferences::PrefsError;

use crate::failure::AppFailure;
use crate::features::pro::data::entitlement_cache::EntitlementCache;
use crate::features::pro::domain::entities::{
    Entitlement, PRO_PRODUCT_ID, ProOffer, PurchaseOutcome, RestoreResult,
};
use crate::features::pro::domain::repositories::EntitlementRepository;

/// How long a purchase waits for the store's own answer before giving up on
/// this attempt. Generous rather than tuned: the wait spans a store sheet the
/// user has to deal with, including a bank approval prompt. Giving up here
/// abandons the wait, not the purchase — the listener stays registered, and a
/// late outcome is still picked up by the next refresh.
const PURCHASE_WAIT: Duration = Duration::from_secs(600);

/// Play's own flag for a purchase it has taken but not yet acknowledged.
const KEY_ACKNOWLEDGED: &str = "isAcknowledgedAndroid";

/// StoreKit's stamp on a refunded or revoked transaction.
const KEY_REVOKED: &str = "revocationDateIOS";

// ---------------------------------------------------------------------------
// The store seam
// ---------------------------------------------------------------------------

/// The eight `frust_iap::Iap` calls this repository makes. See the [module
/// docs](self).
pub(crate) trait StoreApi {
    fn init_connection(&self, config: Option<serde_json::Value>) -> Result<bool, IapError>;
    fn fetch_products(&self, request: ProductRequest) -> Result<FetchProductsResult, IapError>;
    fn get_available_purchases(
        &self,
        options: Option<PurchaseOptions>,
    ) -> Result<Vec<Purchase>, IapError>;
    fn request_purchase(&self, props: RequestPurchaseProps) -> Result<(), IapError>;
    fn finish_transaction(
        &self,
        purchase: PurchaseInput,
        is_consumable: Option<bool>,
    ) -> Result<(), IapError>;
    fn restore_purchases(&self) -> Result<(), IapError>;
    fn get_pending_transactions(&self) -> Result<Vec<Purchase>, IapError>;
    fn set_purchase_listener(
        &self,
        callback: Box<dyn Fn(IapEvent) + Send + Sync>,
    ) -> ListenerHandle;
}

/// The platform store — one associated function per member, no state of its
/// own (the plugin holds the single process-wide connection).
pub(crate) struct PlayStore;

impl StoreApi for PlayStore {
    fn init_connection(&self, config: Option<serde_json::Value>) -> Result<bool, IapError> {
        Iap::init_connection(config)
    }

    fn fetch_products(&self, request: ProductRequest) -> Result<FetchProductsResult, IapError> {
        Iap::fetch_products(request)
    }

    fn get_available_purchases(
        &self,
        options: Option<PurchaseOptions>,
    ) -> Result<Vec<Purchase>, IapError> {
        Iap::get_available_purchases(options)
    }

    fn request_purchase(&self, props: RequestPurchaseProps) -> Result<(), IapError> {
        Iap::request_purchase(props)
    }

    fn finish_transaction(
        &self,
        purchase: PurchaseInput,
        is_consumable: Option<bool>,
    ) -> Result<(), IapError> {
        Iap::finish_transaction(purchase, is_consumable)
    }

    fn restore_purchases(&self) -> Result<(), IapError> {
        Iap::restore_purchases()
    }

    fn get_pending_transactions(&self) -> Result<Vec<Purchase>, IapError> {
        Iap::get_pending_transactions()
    }

    fn set_purchase_listener(
        &self,
        callback: Box<dyn Fn(IapEvent) + Send + Sync>,
    ) -> ListenerHandle {
        Iap::set_purchase_listener(callback)
    }
}

/// Why a store call produced no answer: a platform that cannot sell at all, or
/// a real failure already logged by its code.
enum StoreGap {
    Unavailable,
    Failed(AppFailure),
}

/// Why a purchase request ended the attempt on the spot, rather than leaving
/// the answer to the listener. See
/// [`IapEntitlementRepository::classify_request`].
enum RequestGap {
    /// Nothing to sell through here at all.
    Unavailable,
    /// The store refused, ahead of any purchase flow — its own code names the
    /// outcome, so [`IapEntitlementRepository::refused`] reads it exactly as it
    /// reads a refusal that arrived as an event.
    Refused(IapPurchaseError),
    /// The request never reached the store, carrying what the user is told.
    NeverSent(&'static str),
}

// ---------------------------------------------------------------------------
// The repository
// ---------------------------------------------------------------------------

/// The store-backed entitlement repository. See the [module docs](self).
pub(crate) struct IapEntitlementRepository<S> {
    store: Arc<S>,
    cache: EntitlementCache,
    /// Purchase events, as the registered listener hands them over.
    events: Arc<Mutex<Receiver<IapEvent>>>,
    /// Held for this repository's whole life — dropping it unregisters.
    _listener: ListenerHandle,
    /// Latched by the first `NotAvailable` answer (see the [module docs](self)).
    unavailable: AtomicBool,
    /// Whether the store connection has been opened since the last refusal.
    connected: AtomicBool,
    event_wait: Duration,
    /// How often [`Self::await_outcome`] was entered. Test-only bookkeeping,
    /// so a case whose whole point is that it answered without waiting can
    /// assert that structurally instead of by the clock — nothing reads it in
    /// a shipped build and no behaviour depends on it.
    #[cfg(test)]
    waits: AtomicUsize,
}

/// The production repository — the only shape the app itself builds.
pub(crate) type PlayEntitlementRepository = IapEntitlementRepository<PlayStore>;

impl PlayEntitlementRepository {
    /// Open the platform store's cache and register the purchase listener.
    /// Reaches no store: the connection itself is opened lazily, on the first
    /// call that needs it, so a launch never blocks on billing.
    ///
    /// # Errors
    /// [`PrefsError`] when the preferences store behind the cache cannot be
    /// opened — see [`EntitlementCache::standard`].
    pub(crate) fn standard() -> Result<Self, PrefsError> {
        Ok(Self::new(PlayStore, EntitlementCache::standard()?))
    }
}

impl<S: StoreApi> IapEntitlementRepository<S> {
    pub(crate) fn new(store: S, cache: EntitlementCache) -> Self {
        Self::with_event_wait(store, cache, PURCHASE_WAIT)
    }

    fn with_event_wait(store: S, cache: EntitlementCache, event_wait: Duration) -> Self {
        let (sender, receiver) = mpsc::channel();
        let listener = store.set_purchase_listener(Box::new(move |event| {
            // The whole callback: no blocking, no store call, no signal write —
            // this runs on the plugin's shared event-delivery thread. A send
            // that fails means the receiving repository is already gone, which
            // races this handle's own drop, so it is ignored rather than
            // unwrapped.
            let _ = sender.send(event);
        }));
        Self {
            store: Arc::new(store),
            cache,
            events: Arc::new(Mutex::new(receiver)),
            _listener: listener,
            unavailable: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            event_wait,
            #[cfg(test)]
            waits: AtomicUsize::new(0),
        }
    }
}

impl<S: StoreApi + Send + Sync + 'static> IapEntitlementRepository<S> {
    /// Opens the connection, runs one store call off the caller's thread, and
    /// retries it exactly once if the store reports the connection dropped
    /// underneath it. `init_connection` is idempotent, so re-opening is cheap
    /// when it turns out nothing had dropped.
    async fn call<R, F>(&self, work: F) -> Result<R, StoreGap>
    where
        R: Send + 'static,
        F: Fn(&S) -> Result<R, IapError> + Clone + Send + 'static,
    {
        self.ensure_connected().await?;
        match self.once(work.clone()).await? {
            Ok(value) => Ok(value),
            Err(IapError::NotConnected) => {
                self.connected.store(false, Ordering::Relaxed);
                self.ensure_connected().await?;
                self.once(work).await?.map_err(|err| self.classify(err))
            }
            Err(err) => Err(self.classify(err)),
        }
    }

    /// One store round trip, off the caller's thread — every call but
    /// `request_purchase` blocks, and the platform's UI thread must never park
    /// on one.
    async fn once<R, F>(&self, work: F) -> Result<Result<R, IapError>, StoreGap>
    where
        R: Send + 'static,
        F: FnOnce(&S) -> Result<R, IapError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        off_thread(move || work(&store))
            .await
            .map_err(StoreGap::Failed)
    }

    /// Opens the store connection at most once. The latched
    /// [`Self::unavailable`] flag is what stops a platform with no store from
    /// asking — and logging — again on every gate.
    async fn ensure_connected(&self) -> Result<(), StoreGap> {
        if self.unavailable.load(Ordering::Relaxed) {
            return Err(StoreGap::Unavailable);
        }
        if self.connected.load(Ordering::Relaxed) {
            return Ok(());
        }
        match self.once(|store| store.init_connection(None)).await? {
            Ok(_) => {
                self.connected.store(true, Ordering::Relaxed);
                Ok(())
            }
            Err(err) => Err(self.classify(err)),
        }
    }

    /// Turns a store error into a gap, logging it by code. The first
    /// `NotAvailable` also latches the flag, so exactly one line is logged per
    /// process for a platform that can never sell.
    fn classify(&self, err: IapError) -> StoreGap {
        if let IapError::NotAvailable(reason) = &err {
            if !self.unavailable.swap(true, Ordering::Relaxed) {
                log::info!("pro entitlement: no store to sell through here ({reason:?})");
            }
            return StoreGap::Unavailable;
        }
        let code = store_code(&err);
        log::warn!("pro entitlement: the store call failed: {code}");
        StoreGap::Failed(AppFailure::Internal(code))
    }

    /// Asks the store to start this app's purchase flow, re-opening a
    /// connection that dropped underneath the call.
    ///
    /// [`Self::call`]'s `NotConnected` shape without its `off_thread` hop:
    /// `request_purchase` is the plugin's one non-blocking, UI-thread-callable
    /// member, so there is no blocking wait to move off this thread. Clearing
    /// [`Self::connected`] is the load-bearing half — the flag says "connected"
    /// until something clears it, so a service Play dropped underneath this
    /// repository would otherwise refuse every later attempt the same way.
    ///
    /// `Ok(())` means the answer is the listener's to give.
    async fn request(&self) -> Result<(), RequestGap> {
        match self.store.request_purchase(purchase_props()) {
            Ok(()) => Ok(()),
            Err(IapError::NotConnected) => {
                self.connected.store(false, Ordering::Relaxed);
                self.ensure_connected().await.map_err(reconnect_gap)?;
                match self.store.request_purchase(purchase_props()) {
                    Ok(()) => Ok(()),
                    // Exactly one retry: a connection that reports itself open
                    // and refuses anyway is not going to answer this attempt.
                    Err(err) => self.classify_request(err),
                }
            }
            Err(err) => self.classify_request(err),
        }
    }

    /// What an `Err` from [`StoreApi::request_purchase`] means for the attempt
    /// behind it — the one call whose failure is not an outcome by itself.
    ///
    /// Its own wait is a dispatch ack rather than the purchase, and neither
    /// backend takes back a request it already issued, so a flow that started
    /// keeps running past the `Err` (`plugins/iap/README.md`, *An `Err` from
    /// `request_purchase` doesn't prove the flow never started*). That
    /// "unknown, not failed" reading covers one variant. Every other one is
    /// raised before anything is dispatched, where waiting for an event means
    /// waiting for an event that can never come:
    ///
    /// | [`IapError`] | Where it comes from | This attempt |
    /// |---|---|---|
    /// | `Platform` | ambiguous — the ack timing out *or* glue failing before dispatch (below) | **waits** |
    /// | `Store` | the host's synchronous throws ahead of its detached flow (below) | the code's own outcome, [`Self::refused`] |
    /// | `NotAvailable` | a target with no billing backend | `Unavailable`, latched |
    /// | `NotConnected` | the connection guard — [`Self::request`] has already re-opened and retried once by the time one reaches here | the store cannot be reached |
    /// | `PlatformNotInitialized`, `NotSupportedOnPlatform` | the readiness guards, likewise before any platform work | no purchases on this device |
    /// | `EventThread`, `UiThread`, `Serialization` | a defect on this side: the wrong thread, or a request that would not encode | a generic failure |
    ///
    /// `Platform` is the ambiguous row, and why that arm waits: it carries the
    /// dispatch ack timing out or a hop that failed after issuing the call
    /// (`android.rs`'s `ACK_TIMEOUT`, `apple.rs`'s `await_answer`), but equally
    /// a glue failure raised *before* anything was dispatched (`map_host_error`'s
    /// synthetic and unparseable arms; `bridge_error`'s code-less arm). Nothing
    /// in the payload tells the two apart from this side, so "the sheet may be
    /// open" holds for some of them and not others — the wait costs one attempt,
    /// dropping a purchase already in flight costs the user money (a plugin
    /// follow-up to separate the two is filed).
    ///
    /// `Store` is `FrustIapHost.kt`'s `requestPurchase` branch through
    /// `map_host_error`: a request failing the spec's own invariant, an openiap
    /// module exposing no `requestPurchase` handler, no current Activity —
    /// `developer-error` / `feature-not-supported` / `activity-unavailable`.
    /// iOS reaches this row only for a throw that already *is* a
    /// `PurchaseError`: `FrustIapBridge.swift`'s `errorJson` keeps the spec
    /// shape for those alone, so the `DecodingError` its synchronous
    /// `requestPurchaseProps` decode actually raises degrades to the code-less
    /// shape and lands on the `Platform` row instead.
    fn classify_request(&self, err: IapError) -> Result<(), RequestGap> {
        match err {
            IapError::Platform(_) => {
                log::info!(
                    "pro entitlement: the store took the request without acknowledging it; \
                     waiting for its event"
                );
                Ok(())
            }
            IapError::Store(refusal) => Err(RequestGap::Refused(refusal)),
            // Everything left reached no store: `classify` logs it by code and
            // latches a platform that can never sell, exactly as it does for a
            // blocking call's own failure.
            other => {
                let message = never_sent_message(&other);
                Err(match self.classify(other) {
                    StoreGap::Unavailable => RequestGap::Unavailable,
                    StoreGap::Failed(_) => RequestGap::NeverSent(message),
                })
            }
        }
    }

    /// What the account's own rows entitle this install to, settling a Purchased
    /// row the store has not acknowledged yet on the way past.
    async fn entitlement_of(&self, rows: Vec<Purchase>) -> Entitlement {
        let mut pending = false;
        for row in rows.iter().filter(|row| row.product_id == PRO_PRODUCT_ID) {
            match row.purchase_state {
                PurchaseState::Purchased => {
                    if awaits_acknowledgement(row) {
                        self.settle(row).await;
                    }
                    return Entitlement::Pro;
                }
                PurchaseState::Pending => pending = true,
                PurchaseState::Unknown => {}
            }
        }
        if pending {
            Entitlement::Pending
        } else {
            Entitlement::Free
        }
    }

    /// StoreKit's unfinished transactions for this product: a purchase that
    /// completed while the app was not running to finish it. The active query
    /// can miss one on a cold launch (seen under Xcode's StoreKit testing), and
    /// nothing else here finishes it, so it would be replayed forever. Play has
    /// no such list; any failure reads as none, since the active query already
    /// answered.
    async fn unfinished_pro(&self) -> Vec<Purchase> {
        let rows = match self.once(|store| store.get_pending_transactions()).await {
            Ok(Ok(rows)) => rows,
            Ok(Err(IapError::NotSupportedOnPlatform)) | Err(_) => return Vec::new(),
            Ok(Err(err)) => {
                let _ = self.classify(err);
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter(|row| {
                row.product_id == PRO_PRODUCT_ID
                    && matches!(row.purchase_state, PurchaseState::Purchased)
            })
            .collect()
    }

    /// Acknowledge a purchase the entitlement is already granted off. A failure
    /// is logged by [`Self::classify`] and changes nothing: the store keeps
    /// re-delivering an unsettled row, so the next refresh tries again.
    async fn settle(&self, purchase: &Purchase) {
        let input = settlement(purchase);
        let _ = self
            .call(move |store| store.finish_transaction(input.clone(), None))
            .await;
    }

    /// Throws away anything the listener queued before this attempt — a stale
    /// outcome must never be read as this purchase's answer.
    fn drain_stale_events(&self) {
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        while events.try_recv().is_ok() {}
    }

    /// Waits for the store's own verdict on this product, ignoring events for
    /// anything else. Returns [`PurchaseOutcome::Failed`] when the wait runs
    /// out — the listener stays registered either way.
    async fn await_outcome(&self) -> Result<PurchaseOutcome, AppFailure> {
        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + self.event_wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(no_answer());
            }
            let events = Arc::clone(&self.events);
            let received = off_thread(move || {
                events
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv_timeout(remaining)
            })
            .await?;
            match received {
                Ok(IapEvent::PurchaseUpdated(purchase))
                    if purchase.product_id == PRO_PRODUCT_ID =>
                {
                    return Ok(self.landed(purchase).await);
                }
                // A refusal names the product only when the store knew which
                // one it was refusing; a plain cancel often carries none, and
                // this app has exactly one product in flight either way.
                Ok(IapEvent::PurchaseError(err))
                    if err.product_id.as_deref().unwrap_or(PRO_PRODUCT_ID) == PRO_PRODUCT_ID =>
                {
                    return self.refused(err).await;
                }
                Ok(_) => {}
                Err(_) => return Ok(no_answer()),
            }
        }
    }

    /// A purchase event for this product: grant first, settle second.
    async fn landed(&self, purchase: Purchase) -> PurchaseOutcome {
        match purchase.purchase_state {
            PurchaseState::Purchased => {
                self.cache.write(Entitlement::Pro);
                self.settle(&purchase).await;
                PurchaseOutcome::Purchased
            }
            PurchaseState::Pending => PurchaseOutcome::Pending,
            PurchaseState::Unknown => {
                log::warn!("pro entitlement: the store reported a purchase in an unknown state");
                PurchaseOutcome::Failed(purchase_message(IapErrorCode::Unknown).to_string())
            }
        }
    }

    /// A refusal event for this product.
    async fn refused(&self, err: IapPurchaseError) -> Result<PurchaseOutcome, AppFailure> {
        match err.code {
            // A dismissed sheet is a normal outcome, not a failure to report.
            IapErrorCode::UserCancelled => Ok(PurchaseOutcome::Cancelled),
            // The account already owns it — only the entitlement query can say
            // so, and it settles the row while it is there.
            IapErrorCode::AlreadyOwned => {
                let entitlement = self.refresh().await?;
                Ok(if entitlement.is_pro() {
                    PurchaseOutcome::Purchased
                } else {
                    PurchaseOutcome::Failed(
                        purchase_message(IapErrorCode::AlreadyOwned).to_string(),
                    )
                })
            }
            code => {
                log::warn!("pro entitlement: the store refused the purchase: {code:?}");
                Ok(PurchaseOutcome::Failed(purchase_message(code).to_string()))
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl<S: StoreApi + Send + Sync + 'static> EntitlementRepository for IapEntitlementRepository<S> {
    fn cached(&self) -> Entitlement {
        self.cache.read()
    }

    async fn refresh(&self) -> Result<Entitlement, AppFailure> {
        match self
            .call(|store| store.get_available_purchases(Some(active_purchases_only())))
            .await
        {
            Ok(mut rows) => {
                let unfinished = self.unfinished_pro().await;
                for row in unfinished.iter().filter(|row| !is_revoked(row)) {
                    if !rows.iter().any(|owned| owned.id == row.id) {
                        rows.push(row.clone());
                    }
                }
                let entitlement = self.entitlement_of(rows).await;
                // Finished after the grant, revoked ones included: StoreKit
                // replays an unfinished transaction at every launch until then.
                for row in &unfinished {
                    self.settle(row).await;
                }
                self.cache.write(entitlement);
                Ok(entitlement)
            }
            // Nothing to ask, so the last known answer stands.
            Err(StoreGap::Unavailable) => Ok(self.cache.read()),
            Err(StoreGap::Failed(failure)) => Err(failure),
        }
    }

    async fn offer(&self) -> Result<Option<ProOffer>, AppFailure> {
        let request = ProductRequest {
            skus: vec![PRO_PRODUCT_ID.to_string()],
            kind: ProductQueryType::InApp,
        };
        match self
            .call(move |store| store.fetch_products(request.clone()))
            .await
        {
            // A SKU the store doesn't know is omitted from the answer rather
            // than reported — no product means nothing to sell, not a failure.
            Ok(result) => Ok(offered_product(result).map(|product| ProOffer {
                product_id: product.id,
                title: product.title,
                description: product.description,
                display_price: product.display_price,
            })),
            Err(StoreGap::Unavailable) => Ok(None),
            Err(StoreGap::Failed(failure)) => Err(failure),
        }
    }

    async fn purchase(&self) -> Result<PurchaseOutcome, AppFailure> {
        match self.ensure_connected().await {
            Ok(()) => {}
            Err(StoreGap::Unavailable) => return Ok(PurchaseOutcome::Unavailable),
            Err(StoreGap::Failed(failure)) => return Err(failure),
        }
        self.drain_stale_events();
        match self.request().await {
            // The store has the request — or took it and never acknowledged
            // it, which from here is the same thing: its listener owns the
            // answer either way.
            Ok(()) => self.await_outcome().await,
            Err(RequestGap::Unavailable) => Ok(PurchaseOutcome::Unavailable),
            Err(RequestGap::Refused(refusal)) => self.refused(refusal).await,
            Err(RequestGap::NeverSent(message)) => Ok(PurchaseOutcome::Failed(message.to_string())),
        }
    }

    async fn restore(&self) -> Result<RestoreResult, AppFailure> {
        // Recovered purchases surface through the entitlement query below, not
        // through the sync call's own return — but only a store that answered
        // may say the account owns nothing: no store at all is `Unavailable`,
        // and a sync that failed followed by an empty query is that failure,
        // never an empty account.
        let synced = self.call(|store| store.restore_purchases()).await;
        if let Err(StoreGap::Unavailable) = synced {
            return Ok(RestoreResult::Unavailable);
        }
        let entitlement = self.refresh().await?;
        match (synced, entitlement) {
            (Err(StoreGap::Failed(failure)), Entitlement::Free) => Err(failure),
            (_, entitlement) => Ok(RestoreResult::Found(entitlement)),
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Hands `work` to a background thread, since every store call it wraps blocks
/// on the store.
///
/// The `cfg(test)` arm runs it inline: a headless unit-test process never
/// initializes the reactive runtime `frust::spawn_blocking` requires, and the
/// fake store it drives never actually blocks.
#[cfg(not(test))]
async fn off_thread<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, AppFailure> {
    spawn_blocking(work).await.map_err(|_| {
        log::warn!("pro entitlement: the store call did not finish");
        AppFailure::Internal("the store call did not finish".to_string())
    })
}

#[cfg(test)]
async fn off_thread<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, AppFailure> {
    Ok(work())
}

/// The purchase request for this app's one product, per platform. `frust-iap`
/// carries these payloads as raw JSON by design: Play takes a `skus` list,
/// StoreKit a single `sku`, and every other field of either shape is optional.
fn purchase_props() -> RequestPurchaseProps {
    RequestPurchaseProps {
        request_purchase: Some(RequestPurchasePropsByPlatforms {
            apple: Some(serde_json::json!({ "sku": PRO_PRODUCT_ID })),
            google: Some(serde_json::json!({ "skus": [PRO_PRODUCT_ID] })),
        }),
        request_subscription: None,
        kind: ProductQueryType::InApp,
    }
}

/// Whether Play has taken this purchase without acknowledging it yet. Absent on
/// iOS, where an unfinished transaction is simply re-delivered forever.
fn awaits_acknowledgement(purchase: &Purchase) -> bool {
    purchase
        .extra
        .get(KEY_ACKNOWLEDGED)
        .and_then(|flag| flag.as_bool())
        == Some(false)
}

/// Whether StoreKit refunded or revoked this transaction.
fn is_revoked(purchase: &Purchase) -> bool {
    purchase
        .extra
        .get(KEY_REVOKED)
        .is_some_and(|stamp| !stamp.is_null())
}

/// Enough of a purchase to identify the transaction being settled.
fn settlement(purchase: &Purchase) -> PurchaseInput {
    PurchaseInput {
        id: purchase.id.clone(),
        product_id: purchase.product_id.clone(),
        ids: purchase.ids.clone(),
        transaction_date: purchase.transaction_date,
        purchase_token: purchase.purchase_token.clone(),
        store: Some(purchase.store),
        quantity: purchase.quantity,
        purchase_state: purchase.purchase_state,
        is_auto_renewing: purchase.is_auto_renewing,
    }
}

/// The purchase query every entitlement check makes: what the account holds
/// now. Unasked, StoreKit's side answers from the full transaction history
/// (`Transaction.all`), where a refunded or revoked purchase still reads as
/// purchased, so a refunded iOS user would keep Pro. Play ignores the flag.
fn active_purchases_only() -> PurchaseOptions {
    PurchaseOptions {
        only_include_active_items_ios: Some(true),
        ..PurchaseOptions::default()
    }
}

/// This app's product in a fetch answer, if the store knows it. A subscriptions
/// envelope can't hold it — the query asks for an in-app product.
fn offered_product(result: FetchProductsResult) -> Option<Product> {
    let products = match result {
        FetchProductsResult::Products(products) | FetchProductsResult::All(products) => products,
        FetchProductsResult::Subscriptions(_) => Vec::new(),
    };
    products
        .into_iter()
        .find(|product| product.id == PRO_PRODUCT_ID)
}

/// The outcome of a wait that ran out, or a channel that ended.
fn no_answer() -> PurchaseOutcome {
    log::warn!("pro entitlement: the store reported no purchase outcome in time");
    PurchaseOutcome::Failed("The store didn't answer".to_string())
}

/// An [`IapError`]'s own variant name, or for a store refusal the spec code it
/// carries — never the host's message, which can quote a bearer token.
fn store_code(err: &IapError) -> String {
    match err {
        IapError::PlatformNotInitialized => "platform-not-initialized".to_string(),
        IapError::UiThread => "ui-thread".to_string(),
        IapError::EventThread => "event-thread".to_string(),
        IapError::NotConnected => "not-connected".to_string(),
        IapError::NotSupportedOnPlatform => "not-supported-on-platform".to_string(),
        IapError::Store(refusal) => format!("store {:?}", refusal.code),
        IapError::Serialization(_) => "serialization".to_string(),
        IapError::Platform(_) => "platform".to_string(),
        IapError::NotAvailable(reason) => format!("not-available {reason:?}"),
        _ => "unrecognized".to_string(),
    }
}

/// A failure to re-open the connection under a purchase request, as that
/// attempt's own end: nothing was dispatched, so no event can answer it.
fn reconnect_gap(gap: StoreGap) -> RequestGap {
    match gap {
        StoreGap::Unavailable => RequestGap::Unavailable,
        StoreGap::Failed(_) => {
            RequestGap::NeverSent(purchase_message(IapErrorCode::ServiceDisconnected))
        }
    }
}

/// What the user is told about a request that never reached the store — the
/// same code-derived vocabulary as [`purchase_message`], for the failures that
/// carry no store code of their own. The mapping is
/// [`IapEntitlementRepository::classify_request`]'s table.
fn never_sent_message(err: &IapError) -> &'static str {
    let code = match err {
        IapError::NotConnected => IapErrorCode::ServiceDisconnected,
        IapError::PlatformNotInitialized | IapError::NotSupportedOnPlatform => {
            IapErrorCode::IapNotAvailable
        }
        _ => IapErrorCode::Unknown,
    };
    purchase_message(code)
}

/// What a failed purchase tells the user, derived from the store's code — its
/// message is the host's own text and never shown.
fn purchase_message(code: IapErrorCode) -> &'static str {
    match code {
        IapErrorCode::ItemUnavailable | IapErrorCode::SkuNotFound => {
            "This purchase isn't available right now."
        }
        IapErrorCode::NetworkError
        | IapErrorCode::ServiceDisconnected
        | IapErrorCode::ServiceTimeout
        | IapErrorCode::ConnectionClosed => "The store couldn't be reached.",
        IapErrorCode::BillingUnavailable
        | IapErrorCode::IapNotAvailable
        | IapErrorCode::FeatureNotSupported => "Purchases aren't available on this device.",
        IapErrorCode::AlreadyOwned => "This purchase is already on your account.",
        _ => "The store couldn't complete the purchase.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use frust_iap::Unavailability;

    /// A scriptable store: owned rows, a product list, one event delivered when
    /// a purchase is requested, and per-call refusals.
    struct FakeStore {
        state: Mutex<FakeState>,
        listener: Mutex<Option<SharedListener>>,
    }

    /// The registered callback, kept so a scripted event can be delivered
    /// through it.
    type SharedListener = Arc<dyn Fn(IapEvent) + Send + Sync>;

    #[derive(Default)]
    struct FakeState {
        owned: Vec<Purchase>,
        products: Vec<Product>,
        on_request: Option<IapEvent>,
        /// Every call answers `NotAvailable`, as desktop does.
        unavailable: bool,
        /// `request_purchase` refuses, without withholding `on_request`.
        request_fails: bool,
        /// `restore_purchases` refuses; the query after it still answers.
        restore_fails: bool,
        /// One pre-dispatch refusal per `request_purchase` call, in order.
        /// Nothing reaches the store on these, so no event follows them.
        request_errors: Vec<IapError>,
        settled: Vec<String>,
        /// The options of every purchase query, in order.
        purchase_queries: Vec<Option<PurchaseOptions>>,
        /// StoreKit's unfinished transactions; `None` answers as Play does.
        unfinished: Option<Vec<Purchase>>,
        restore_calls: usize,
        /// How often the connection was opened — a second one is a latch that
        /// was cleared and re-established.
        connect_calls: usize,
    }

    impl FakeStore {
        fn new() -> Self {
            Self {
                state: Mutex::new(FakeState::default()),
                listener: Mutex::new(None),
            }
        }

        fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn gap(&self) -> Option<IapError> {
            self.state()
                .unavailable
                .then_some(IapError::NotAvailable(Unavailability::UnsupportedPlatform))
        }

        /// The next scripted pre-dispatch refusal, if one is left.
        fn next_request_error(&self) -> Option<IapError> {
            let mut state = self.state();
            (!state.request_errors.is_empty()).then(|| state.request_errors.remove(0))
        }
    }

    impl StoreApi for FakeStore {
        fn init_connection(&self, _config: Option<serde_json::Value>) -> Result<bool, IapError> {
            match self.gap() {
                Some(err) => Err(err),
                None => {
                    self.state().connect_calls += 1;
                    Ok(true)
                }
            }
        }

        fn fetch_products(
            &self,
            _request: ProductRequest,
        ) -> Result<FetchProductsResult, IapError> {
            match self.gap() {
                Some(err) => Err(err),
                None => Ok(FetchProductsResult::Products(self.state().products.clone())),
            }
        }

        fn get_available_purchases(
            &self,
            options: Option<PurchaseOptions>,
        ) -> Result<Vec<Purchase>, IapError> {
            self.state().purchase_queries.push(options);
            match self.gap() {
                Some(err) => Err(err),
                None => Ok(self.state().owned.clone()),
            }
        }

        fn get_pending_transactions(&self) -> Result<Vec<Purchase>, IapError> {
            match self.gap() {
                Some(err) => Err(err),
                None => self
                    .state()
                    .unfinished
                    .clone()
                    .ok_or(IapError::NotSupportedOnPlatform),
            }
        }

        fn request_purchase(&self, _props: RequestPurchaseProps) -> Result<(), IapError> {
            if let Some(err) = self.gap() {
                return Err(err);
            }
            // A refusal raised ahead of the purchase flow — a dropped
            // connection, a host that threw before dispatching — reaches no
            // store, so nothing is ever delivered for it.
            if let Some(err) = self.next_request_error() {
                return Err(err);
            }
            // The store's own answer arrives on the listener, whether or not
            // the request itself was acknowledged.
            let event = self.state().on_request.clone();
            if let Some(event) = event {
                let listener = self
                    .listener
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                if let Some(listener) = listener {
                    listener(event);
                }
            }
            if self.state().request_fails {
                return Err(IapError::Platform("ack timed out".to_string()));
            }
            Ok(())
        }

        fn finish_transaction(
            &self,
            purchase: PurchaseInput,
            _is_consumable: Option<bool>,
        ) -> Result<(), IapError> {
            match self.gap() {
                Some(err) => Err(err),
                None => {
                    self.state().settled.push(purchase.id);
                    Ok(())
                }
            }
        }

        fn restore_purchases(&self) -> Result<(), IapError> {
            if let Some(err) = self.gap() {
                return Err(err);
            }
            let mut state = self.state();
            state.restore_calls += 1;
            if state.restore_fails {
                return Err(IapError::Platform("sync failed".to_string()));
            }
            Ok(())
        }

        fn set_purchase_listener(
            &self,
            callback: Box<dyn Fn(IapEvent) + Send + Sync>,
        ) -> ListenerHandle {
            let shared: SharedListener = Arc::from(callback);
            *self.listener.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(Arc::clone(&shared));
            // Registration is process-global bookkeeping that reaches no store
            // and never fires off-device, so the plugin can mint a real handle
            // here while the fake delivers its own scripted events.
            Iap::set_purchase_listener(Box::new(move |event| shared(event)))
        }
    }

    /// A wait short enough that the timeout case is a test, not a pause.
    const TEST_WAIT: Duration = Duration::from_millis(30);

    /// A wait no fail-fast case may ever reach, long enough that a regression
    /// into one is a stall rather than a hiccup — 30s against the sub-second
    /// work these cases actually do, so the sanity bound below survives a
    /// heavily loaded machine while a real dead wait still blows through it.
    const UNMISSABLE_WAIT: Duration = Duration::from_secs(30);

    fn repository() -> IapEntitlementRepository<FakeStore> {
        repository_waiting(TEST_WAIT)
    }

    fn repository_waiting(wait: Duration) -> IapEntitlementRepository<FakeStore> {
        IapEntitlementRepository::with_event_wait(
            FakeStore::new(),
            EntitlementCache::in_memory(),
            wait,
        )
    }

    /// Runs `purchase()` and reports how long it took, for the cases whose
    /// point is that they answer without waiting for an event.
    fn timed_purchase(
        repo: &IapEntitlementRepository<FakeStore>,
    ) -> (Result<PurchaseOutcome, AppFailure>, Duration) {
        let started = Instant::now();
        let outcome = block_on(repo.purchase());
        (outcome, started.elapsed())
    }

    /// Asserts an attempt ended on its own answer rather than on the listener
    /// wait.
    ///
    /// The load-bearing half is structural: [`IapEntitlementRepository::waits`]
    /// counts entries into `await_outcome`, so this reads the property itself
    /// rather than inferring it from a stopwatch a loaded machine can stretch.
    /// The elapsed bound stays as a deliberately generous sanity check — half
    /// of the repository's own `event_wait`, which a dead wait (the whole of
    /// it) can never fit inside no matter how fast the machine is.
    fn assert_answered_without_waiting(
        repo: &IapEntitlementRepository<FakeStore>,
        elapsed: Duration,
    ) {
        assert_eq!(
            repo.waits.load(Ordering::Relaxed),
            0,
            "a request that never reached the store must not wait for an event that cannot come"
        );
        assert!(
            elapsed < repo.event_wait / 2,
            "answered in {elapsed:?}, past half of {:?}",
            repo.event_wait
        );
    }

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    fn row(state: PurchaseState, acknowledged: Option<bool>) -> Purchase {
        let mut extra = serde_json::Map::new();
        if let Some(acknowledged) = acknowledged {
            extra.insert(KEY_ACKNOWLEDGED.to_string(), acknowledged.into());
        }
        Purchase {
            id: "txn-1".to_string(),
            product_id: PRO_PRODUCT_ID.to_string(),
            ids: None,
            transaction_date: 1.0,
            purchase_token: Some("token".to_string()),
            store: frust_iap::IapStore::Google,
            quantity: 1,
            purchase_state: state,
            is_auto_renewing: false,
            current_plan_id: None,
            extra,
        }
    }

    fn product(id: &str) -> Product {
        Product {
            id: id.to_string(),
            title: "Muxr Pro".to_string(),
            description: "Unlimited servers".to_string(),
            price: Some(4.99),
            display_price: "$4.99".to_string(),
            currency: "USD".to_string(),
            kind: frust_iap::ProductType::InApp,
            platform: frust_iap::IapPlatform::Android,
            extra: serde_json::Map::new(),
        }
    }

    /// The store's own refusal payload, carrying host text no outcome may ever
    /// quote back.
    fn refusal_payload(code: IapErrorCode) -> IapPurchaseError {
        IapPurchaseError {
            code,
            message: "host text that must never be shown".to_string(),
            product_id: Some(PRO_PRODUCT_ID.to_string()),
        }
    }

    fn refusal(code: IapErrorCode) -> IapEvent {
        IapEvent::PurchaseError(refusal_payload(code))
    }

    #[test]
    fn an_unfinished_purchase_the_active_query_missed_grants_pro_and_is_finished() {
        // A purchase completed while the app was killed: StoreKit lists it as
        // unfinished, and a cold launch's active query may not return it.
        let repo = repository();
        repo.store.state().unfinished = Some(vec![row(PurchaseState::Purchased, None)]);

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert_eq!(repo.store.state().settled, vec!["txn-1".to_string()]);
    }

    #[test]
    fn an_unfinished_purchase_already_owned_is_finished_once() {
        let repo = repository();
        repo.store.state().owned = vec![row(PurchaseState::Purchased, None)];
        repo.store.state().unfinished = Some(vec![row(PurchaseState::Purchased, None)]);

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert_eq!(repo.store.state().settled, vec!["txn-1".to_string()]);
    }

    #[test]
    fn a_revoked_unfinished_purchase_is_finished_but_grants_nothing() {
        let repo = repository();
        let mut revoked = row(PurchaseState::Purchased, None);
        revoked.extra.insert(KEY_REVOKED.to_string(), 1.0.into());
        repo.store.state().unfinished = Some(vec![revoked]);

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Free));
        assert_eq!(repo.store.state().settled, vec!["txn-1".to_string()]);
    }

    #[test]
    fn a_store_without_unfinished_transactions_changes_nothing() {
        // Play answers `NotSupportedOnPlatform`; that is no gap in the check.
        let repo = repository();
        repo.store.state().owned = vec![row(PurchaseState::Purchased, Some(true))];

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert!(repo.store.state().settled.is_empty());
    }

    #[test]
    fn the_entitlement_check_asks_for_active_purchases_only() {
        // StoreKit's default answer is the whole transaction history, where a
        // refunded purchase still reads as purchased.
        let repo = repository();

        let _ = block_on(repo.refresh());

        assert_eq!(
            repo.store.state().purchase_queries,
            vec![Some(PurchaseOptions {
                only_include_active_items_ios: Some(true),
                ..PurchaseOptions::default()
            })]
        );
    }

    #[test]
    fn an_unacknowledged_purchased_row_is_settled_and_grants_pro() {
        let repo = repository();
        repo.store.state().owned = vec![row(PurchaseState::Purchased, Some(false))];

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert_eq!(
            repo.store.state().settled,
            vec!["txn-1".to_string()],
            "Play auto-refunds a row that is never acknowledged"
        );
        assert_eq!(repo.cached(), Entitlement::Pro);
    }

    #[test]
    fn a_pending_row_grants_nothing_yet() {
        let repo = repository();
        repo.store.state().owned = vec![row(PurchaseState::Pending, None)];

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pending));
        assert!(repo.store.state().settled.is_empty());
        assert_eq!(repo.cached(), Entitlement::Free, "Pending caches as Free");
    }

    #[test]
    fn an_account_that_owns_nothing_flips_the_cache_back_to_free() {
        // The refund path: the cache is overwritten by the store's answer
        // rather than merged with it.
        let repo = repository();
        repo.cache.write(Entitlement::Pro);

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Free));
        assert_eq!(repo.cached(), Entitlement::Free);
    }

    #[test]
    fn a_platform_with_no_store_answers_from_the_cache_rather_than_failing() {
        let repo = repository();
        repo.cache.write(Entitlement::Pro);
        repo.store.state().unavailable = true;

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert_eq!(block_on(repo.offer()), Ok(None));
        assert_eq!(
            block_on(repo.purchase()),
            Ok(PurchaseOutcome::Unavailable),
            "the desktop preview has nothing to sell"
        );
    }

    #[test]
    fn a_purchased_event_grants_pro_and_settles_the_transaction() {
        let repo = repository();
        repo.store.state().on_request = Some(IapEvent::PurchaseUpdated(row(
            PurchaseState::Purchased,
            None,
        )));

        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Purchased));
        assert_eq!(repo.cached(), Entitlement::Pro);
        assert_eq!(repo.store.state().settled, vec!["txn-1".to_string()]);
    }

    #[test]
    fn a_dismissed_sheet_is_a_cancel_and_changes_nothing() {
        let repo = repository();
        repo.store.state().on_request = Some(refusal(IapErrorCode::UserCancelled));

        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Cancelled));
        assert_eq!(repo.cached(), Entitlement::Free);
    }

    #[test]
    fn an_already_owned_refusal_is_resolved_by_the_entitlement_query() {
        let repo = repository();
        repo.store.state().on_request = Some(refusal(IapErrorCode::AlreadyOwned));
        repo.store.state().owned = vec![row(PurchaseState::Purchased, None)];

        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Purchased));
        assert_eq!(repo.cached(), Entitlement::Pro);
    }

    #[test]
    fn a_store_that_never_answers_fails_the_attempt_without_a_message_of_its_own() {
        let repo = repository();

        assert_eq!(
            block_on(repo.purchase()),
            Ok(PurchaseOutcome::Failed(
                "The store didn't answer".to_string()
            ))
        );
        assert_eq!(repo.cached(), Entitlement::Free);
    }

    #[test]
    fn a_refusal_shows_a_message_derived_from_the_code_never_the_hosts_own_text() {
        let repo = repository();
        repo.store.state().on_request = Some(refusal(IapErrorCode::ItemUnavailable));

        assert_eq!(
            block_on(repo.purchase()),
            Ok(PurchaseOutcome::Failed(
                "This purchase isn't available right now.".to_string()
            ))
        );
    }

    #[test]
    fn a_sku_the_store_does_not_know_is_omitted_rather_than_reported() {
        let repo = repository();
        repo.store.state().products = vec![product("something_else")];

        assert_eq!(block_on(repo.offer()), Ok(None));

        repo.store.state().products = vec![product(PRO_PRODUCT_ID)];
        let offer = block_on(repo.offer())
            .expect("a store answer")
            .expect("the offer");
        assert_eq!(offer.product_id, PRO_PRODUCT_ID);
        assert_eq!(offer.display_price, "$4.99");
    }

    #[test]
    fn a_request_the_store_never_acknowledged_still_lands_on_its_event() {
        // An `Err` from `request_purchase` is dispatch news, not an outcome:
        // the sheet may already be open, so the listener stays the truth.
        let repo = repository();
        repo.store.state().request_fails = true;
        repo.store.state().on_request = Some(IapEvent::PurchaseUpdated(row(
            PurchaseState::Purchased,
            None,
        )));

        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Purchased));
        assert_eq!(repo.cached(), Entitlement::Pro);
    }

    #[test]
    fn a_connection_dropped_under_the_request_is_re_opened_and_the_purchase_lands() {
        let repo = repository();
        repo.store.state().request_errors = vec![IapError::NotConnected];
        repo.store.state().on_request = Some(IapEvent::PurchaseUpdated(row(
            PurchaseState::Purchased,
            None,
        )));

        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Purchased));
        assert_eq!(
            repo.store.state().connect_calls,
            2,
            "the cached flag has to be cleared and the connection actually re-opened"
        );
        assert!(
            repo.connected.load(Ordering::Relaxed),
            "the re-opened connection is what the next attempt starts from"
        );
        assert_eq!(
            repo.waits.load(Ordering::Relaxed),
            1,
            "this one answers through the listener, which is what makes the fail-fast cases' \
             own count of zero mean something"
        );
        assert_eq!(repo.cached(), Entitlement::Pro);
    }

    #[test]
    fn a_connection_that_stays_dropped_fails_the_attempt_rather_than_waiting() {
        // Nothing was ever dispatched, so no event is coming: waiting here is
        // the dead wait that made every later Upgrade tap cost the same.
        let repo = repository_waiting(UNMISSABLE_WAIT);
        repo.store.state().request_errors = vec![IapError::NotConnected, IapError::NotConnected];

        let (outcome, elapsed) = timed_purchase(&repo);

        assert_eq!(
            outcome,
            Ok(PurchaseOutcome::Failed(
                "The store couldn't be reached.".to_string()
            ))
        );
        assert_answered_without_waiting(&repo, elapsed);
        assert_eq!(
            repo.store.state().connect_calls,
            2,
            "re-opened exactly once — a second refusal ends the attempt"
        );
    }

    #[test]
    fn a_refusal_raised_ahead_of_the_flow_answers_from_its_code_without_waiting() {
        // The host validates the request, resolves the Activity and checks for
        // a purchase handler *before* it detaches the flow, and throws these
        // synchronously — no listener event can ever follow one.
        let repo = repository_waiting(UNMISSABLE_WAIT);
        repo.store.state().request_errors = vec![IapError::Store(refusal_payload(
            IapErrorCode::FeatureNotSupported,
        ))];

        let (outcome, elapsed) = timed_purchase(&repo);

        assert_eq!(
            outcome,
            Ok(PurchaseOutcome::Failed(
                "Purchases aren't available on this device.".to_string()
            )),
            "the message is the code's, never the host's own text"
        );
        assert_answered_without_waiting(&repo, elapsed);
        assert_eq!(repo.cached(), Entitlement::Free);
    }

    #[test]
    fn a_cancel_the_request_itself_reports_is_a_cancel_not_a_wait() {
        let repo = repository_waiting(UNMISSABLE_WAIT);
        repo.store.state().request_errors = vec![IapError::Store(refusal_payload(
            IapErrorCode::UserCancelled,
        ))];

        let (outcome, elapsed) = timed_purchase(&repo);

        assert_eq!(outcome, Ok(PurchaseOutcome::Cancelled));
        assert_answered_without_waiting(&repo, elapsed);
        assert_eq!(repo.cached(), Entitlement::Free);
    }

    #[test]
    fn a_restore_re_reads_the_entitlement_after_asking_the_store_to_sync() {
        let repo = repository();
        repo.store.state().owned = vec![row(PurchaseState::Purchased, None)];

        assert_eq!(
            block_on(repo.restore()),
            Ok(RestoreResult::Found(Entitlement::Pro))
        );
        assert_eq!(repo.store.state().restore_calls, 1);
    }

    #[test]
    fn a_restore_with_no_store_answers_unavailable_never_an_empty_account() {
        let repo = repository();
        repo.store.state().unavailable = true;

        assert_eq!(block_on(repo.restore()), Ok(RestoreResult::Unavailable));
        assert_eq!(repo.store.state().restore_calls, 0, "nothing was asked");
    }

    #[test]
    fn a_restore_whose_sync_failed_over_an_empty_account_is_that_failure() {
        let repo = repository();
        repo.store.state().restore_fails = true;

        assert!(
            block_on(repo.restore()).is_err(),
            "the store did not answer"
        );
        assert_eq!(repo.store.state().restore_calls, 1);
    }

    #[test]
    fn a_restore_whose_sync_failed_still_finds_the_purchase_the_query_reports() {
        let repo = repository();
        repo.store.state().restore_fails = true;
        repo.store.state().owned = vec![row(PurchaseState::Purchased, None)];

        assert_eq!(
            block_on(repo.restore()),
            Ok(RestoreResult::Found(Entitlement::Pro))
        );
    }
}
