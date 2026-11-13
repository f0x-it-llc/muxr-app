//! Store-backed `EntitlementRepository` over `frust-iap`: a purchase is request, event, grant, settle; the listener is the single source of truth.
//! Play auto-refunds an unacknowledged purchase after ~3 days, so each Purchased row is finished once the grant is durable; a failed settle never revokes it.
//! Errors are logged by code, never by payload: a store message can quote the bearer token, so never format an `IapError` with `Display`.

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

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

/// Generous: the wait spans a store sheet and a bank approval prompt. Giving up abandons the wait, not the purchase;
/// a late outcome is picked up by the next refresh.
const PURCHASE_WAIT: Duration = Duration::from_secs(600);

const KEY_ACKNOWLEDGED: &str = "isAcknowledgedAndroid";

const KEY_REVOKED: &str = "revocationDateIOS";

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

enum StoreGap {
    Unavailable,
    Failed(AppFailure),
}

enum RequestGap {
    Unavailable,
    Refused(IapPurchaseError),
    NeverSent(&'static str),
}

pub(crate) struct IapEntitlementRepository<S> {
    store: Arc<S>,
    cache: EntitlementCache,
    changed: Arc<Notify>,
    /// Whether a purchase flow is waiting on the listener, so a refresh leaves the queued events to it.
    purchasing: AtomicBool,
    events: Arc<Mutex<Receiver<IapEvent>>>,
    /// Held for this repository's whole life: dropping it unregisters.
    _listener: ListenerHandle,
    /// Latched by the first `NotAvailable` answer, so the desktop preview asks once and then reads from the cache.
    unavailable: AtomicBool,
    connected: AtomicBool,
    event_wait: Duration,
    /// Test-only: lets a case assert structurally that it answered without waiting.
    #[cfg(test)]
    waits: AtomicUsize,
}

pub(crate) type PlayEntitlementRepository = IapEntitlementRepository<PlayStore>;

impl PlayEntitlementRepository {
    /// Reaches no store: the connection opens lazily, so a launch never blocks on billing.
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
        let changed = Arc::new(Notify::new());
        let raised = Arc::clone(&changed);
        let listener = store.set_purchase_listener(Box::new(move |event| {
            // Runs on the plugin's one event-delivery thread: no blocking, no store call. A failed send means the repository is gone.
            let _ = sender.send(event);
            raised.notify_one();
        }));
        Self {
            store: Arc::new(store),
            cache,
            changed,
            purchasing: AtomicBool::new(false),
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
    /// Retries once if the store reports the connection dropped; `init_connection` is idempotent.
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

    /// Every call but `request_purchase` blocks, and the UI thread must never park on one.
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

    /// The first `NotAvailable` latches the flag, so one line is logged per process.
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

    /// Unlike `call` this skips the `off_thread` hop: `request_purchase` is non-blocking and UI-thread-callable. Clearing `connected` is load-bearing:
    /// the flag says connected until cleared, so a dropped service would refuse every later attempt. `Ok(())` means the answer is the listener's.
    async fn request(&self) -> Result<(), RequestGap> {
        match self.store.request_purchase(purchase_props()) {
            Ok(()) => Ok(()),
            Err(IapError::NotConnected) => {
                self.connected.store(false, Ordering::Relaxed);
                self.ensure_connected().await.map_err(reconnect_gap)?;
                match self.store.request_purchase(purchase_props()) {
                    Ok(()) => Ok(()),
                    // Exactly one retry: a connection that reports itself open and refuses anyway will not answer this attempt.
                    Err(err) => self.classify_request(err),
                }
            }
            Err(err) => self.classify_request(err),
        }
    }

    /// An `Err` here is not an outcome by itself. Only `Platform` (an ack timeout, or glue failing before dispatch, indistinguishable from this side) waits for the
    /// listener, because dropping a purchase already in flight costs the user money. Every other variant is raised before dispatch, so waiting would wait for an event that cannot come:
    /// `Store` maps to the code's own outcome via `refused`, `NotAvailable` to Unavailable (latched), `NotConnected` to unreachable,
    /// the readiness guards to no purchases on this device, and `EventThread`/`UiThread`/`Serialization` to a generic failure.
    /// iOS reaches `Store` only for a throw that is already a `PurchaseError`; its decode errors land on `Platform`.
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
            // Everything left reached no store: log by code and latch a platform that can never sell.
            other => {
                let message = never_sent_message(&other);
                Err(match self.classify(other) {
                    StoreGap::Unavailable => RequestGap::Unavailable,
                    StoreGap::Failed(_) => RequestGap::NeverSent(message),
                })
            }
        }
    }

    async fn entitlement_of(&self, rows: Vec<Purchase>) -> Entitlement {
        let mut pending = false;
        // A refunded or revoked transaction stays in StoreKit's history.
        for row in rows
            .iter()
            .filter(|row| row.product_id == PRO_PRODUCT_ID && !is_revoked(row))
        {
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

    fn queued_pro_updates(&self) -> Vec<Purchase> {
        if self.purchasing.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                IapEvent::PurchaseUpdated(row)
                    if row.product_id == PRO_PRODUCT_ID
                        && matches!(row.purchase_state, PurchaseState::Purchased) =>
                {
                    Some(row)
                }
                _ => None,
            })
            .collect()
    }

    /// StoreKit's unfinished transactions: the purchase query can miss one on a cold launch and nothing else finishes it,
    /// so it would replay forever. Play has none; a failure reads as none.
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

    /// A failed ack is logged and changes nothing: the store re-delivers the row, so the next refresh retries.
    async fn settle(&self, purchase: &Purchase) {
        let input = settlement(purchase);
        let _ = self
            .call(move |store| store.finish_transaction(input.clone(), None))
            .await;
    }

    /// A stale outcome must never be read as this purchase's answer.
    fn drain_stale_events(&self) {
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        while events.try_recv().is_ok() {}
    }

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
                // A refusal names the product only when the store knew which one it was refusing; a plain cancel often carries none.
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

    async fn refused(&self, err: IapPurchaseError) -> Result<PurchaseOutcome, AppFailure> {
        match err.code {
            IapErrorCode::UserCancelled => Ok(PurchaseOutcome::Cancelled),
            // Already owned: only the entitlement query can say so, and it settles the row.
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
            .call(|store| store.get_available_purchases(Some(purchase_history())))
            .await
        {
            Ok(mut rows) => {
                // Also unfinished StoreKit transactions: the query above can miss one right after it lands.
                let mut unsettled = self.queued_pro_updates();
                for row in self.unfinished_pro().await {
                    if !unsettled.iter().any(|queued| queued.id == row.id) {
                        unsettled.push(row);
                    }
                }
                for row in unsettled.iter().filter(|row| !is_revoked(row)) {
                    if !rows.iter().any(|owned| owned.id == row.id) {
                        rows.push(row.clone());
                    }
                }
                let entitlement = self.entitlement_of(rows).await;
                // Finished after the grant, revoked ones included: StoreKit replays an unfinished transaction at every launch.
                for row in &unsettled {
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
            // A SKU the store does not know is omitted from the answer: no product means nothing to sell, not a failure.
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
        self.purchasing.store(true, Ordering::Relaxed);
        let outcome = self.purchase_flow().await;
        self.purchasing.store(false, Ordering::Relaxed);
        outcome
    }

    async fn restore(&self) -> Result<RestoreResult, AppFailure> {
        self.restore_flow().await
    }

    async fn store_changed(&self) {
        self.changed.notified().await;
    }
}

impl<S: StoreApi + Send + Sync + 'static> IapEntitlementRepository<S> {
    async fn purchase_flow(&self) -> Result<PurchaseOutcome, AppFailure> {
        match self.ensure_connected().await {
            Ok(()) => {}
            Err(StoreGap::Unavailable) => return Ok(PurchaseOutcome::Unavailable),
            Err(StoreGap::Failed(failure)) => return Err(failure),
        }
        self.drain_stale_events();
        match self.request().await {
            // Taken but possibly never acknowledged: the listener owns the answer either way.
            Ok(()) => self.await_outcome().await,
            Err(RequestGap::Unavailable) => Ok(PurchaseOutcome::Unavailable),
            Err(RequestGap::Refused(refusal)) => self.refused(refusal).await,
            Err(RequestGap::NeverSent(message)) => Ok(PurchaseOutcome::Failed(message.to_string())),
        }
    }

    async fn restore_flow(&self) -> Result<RestoreResult, AppFailure> {
        // Only a store that answered may say the account owns nothing: no store is `Unavailable`, and a failed sync
        // followed by an empty query is that failure, never an empty account.
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

/// Hands blocking store work to a background thread; the `cfg(test)` arm runs inline since a headless process has no reactive runtime.
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

/// Play takes a `skus` list, StoreKit a single `sku`; `frust-iap` carries both as raw JSON.
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

/// Absent on iOS, where an unfinished transaction is re-delivered forever.
fn awaits_acknowledgement(purchase: &Purchase) -> bool {
    purchase
        .extra
        .get(KEY_ACKNOWLEDGED)
        .and_then(|flag| flag.as_bool())
        == Some(false)
}

fn is_revoked(purchase: &Purchase) -> bool {
    purchase
        .extra
        .get(KEY_REVOKED)
        .is_some_and(|stamp| !stamp.is_null())
}

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

/// StoreKit's full history (`Transaction.all`), revocations filtered by `entitlement_of`: `currentEntitlements` can answer
/// empty for an owned purchase on a cold launch. Play ignores the flag.
fn purchase_history() -> PurchaseOptions {
    PurchaseOptions {
        only_include_active_items_ios: Some(false),
        ..PurchaseOptions::default()
    }
}

/// A subscriptions envelope cannot hold it: the query asks for an in-app product.
fn offered_product(result: FetchProductsResult) -> Option<Product> {
    let products = match result {
        FetchProductsResult::Products(products) | FetchProductsResult::All(products) => products,
        FetchProductsResult::Subscriptions(_) => Vec::new(),
    };
    products
        .into_iter()
        .find(|product| product.id == PRO_PRODUCT_ID)
}

fn no_answer() -> PurchaseOutcome {
    log::warn!("pro entitlement: the store reported no purchase outcome in time");
    PurchaseOutcome::Failed("The store didn't answer".to_string())
}

/// Never the host's message, which can quote a bearer token.
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

fn reconnect_gap(gap: StoreGap) -> RequestGap {
    match gap {
        StoreGap::Unavailable => RequestGap::Unavailable,
        StoreGap::Failed(_) => {
            RequestGap::NeverSent(purchase_message(IapErrorCode::ServiceDisconnected))
        }
    }
}

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

/// Derived from the store's code; the host's message is never shown.
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

    struct FakeStore {
        state: Mutex<FakeState>,
        listener: Mutex<Option<SharedListener>>,
    }

    type SharedListener = Arc<dyn Fn(IapEvent) + Send + Sync>;

    #[derive(Default)]
    struct FakeState {
        owned: Vec<Purchase>,
        products: Vec<Product>,
        on_request: Option<IapEvent>,
        unavailable: bool,
        request_fails: bool,
        restore_fails: bool,
        /// Pre-dispatch refusals reach no store, so no event follows them.
        request_errors: Vec<IapError>,
        settled: Vec<String>,
        purchase_queries: Vec<Option<PurchaseOptions>>,
        unfinished: Option<Vec<Purchase>>,
        restore_calls: usize,
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

        fn deliver(&self, event: IapEvent) {
            let listener = self
                .listener
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(listener) = listener {
                listener(event);
            }
        }

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
            // A refusal raised ahead of the purchase flow reaches no store, so nothing is ever delivered for it.
            if let Some(err) = self.next_request_error() {
                return Err(err);
            }
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
            // Registration is process-global and never fires off-device, so the plugin can mint a real handle here.
            Iap::set_purchase_listener(Box::new(move |event| shared(event)))
        }
    }

    const TEST_WAIT: Duration = Duration::from_millis(30);

    /// Long enough that a dead wait is a stall; 30s against sub-second work keeps the bound valid under load.
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

    fn timed_purchase(
        repo: &IapEntitlementRepository<FakeStore>,
    ) -> (Result<PurchaseOutcome, AppFailure>, Duration) {
        let started = Instant::now();
        let outcome = block_on(repo.purchase());
        (outcome, started.elapsed())
    }

    /// Structural: `waits` counts entries into `await_outcome`. The elapsed bound is a generous sanity check at half of `event_wait`.
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

    /// Host text no outcome may ever quote back.
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
    fn an_update_delivered_outside_a_purchase_grants_pro_and_is_finished() {
        let repo = repository();
        repo.store.deliver(IapEvent::PurchaseUpdated(row(
            PurchaseState::Purchased,
            None,
        )));

        block_on(repo.store_changed());
        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert_eq!(repo.store.state().settled, vec!["txn-1".to_string()]);
    }

    #[test]
    fn a_purchase_in_flight_keeps_its_events_from_a_refresh() {
        let repo = repository();
        repo.store.deliver(IapEvent::PurchaseUpdated(row(
            PurchaseState::Purchased,
            None,
        )));
        repo.purchasing.store(true, Ordering::Relaxed);

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Free));
        assert!(repo.store.state().settled.is_empty());
        assert!(
            repo.events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_recv()
                .is_ok(),
            "the flow's event is still queued for it"
        );
    }

    #[test]
    fn an_unfinished_purchase_the_active_query_missed_grants_pro_and_is_finished() {
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
        let repo = repository();
        repo.store.state().owned = vec![row(PurchaseState::Purchased, Some(true))];

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert!(repo.store.state().settled.is_empty());
    }

    #[test]
    fn the_entitlement_check_reads_the_full_purchase_history() {
        let repo = repository();

        let _ = block_on(repo.refresh());

        assert_eq!(
            repo.store.state().purchase_queries,
            vec![Some(PurchaseOptions {
                only_include_active_items_ios: Some(false),
                ..PurchaseOptions::default()
            })]
        );
    }

    #[test]
    fn a_refunded_purchase_in_the_history_grants_nothing() {
        let repo = repository();
        let mut refunded = row(PurchaseState::Purchased, None);
        refunded.extra.insert(KEY_REVOKED.to_string(), 1.0.into());
        repo.store.state().owned = vec![refunded];

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Free));
    }

    #[test]
    fn a_later_live_purchase_outweighs_a_refunded_one() {
        let repo = repository();
        let mut refunded = row(PurchaseState::Purchased, None);
        refunded.extra.insert(KEY_REVOKED.to_string(), 1.0.into());
        let mut bought_again = row(PurchaseState::Purchased, None);
        bought_again.id = "txn-2".to_string();
        repo.store.state().owned = vec![refunded, bought_again];

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
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
