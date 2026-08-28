//! [`IapEntitlementRepository`] — the real [`EntitlementRepository`], over
//! `frust-iap`'s Play Billing/StoreKit surface and an [`EntitlementCache`].
//!
//! **A purchase is request → event → grant → settle.** `request_purchase` only
//! acknowledges that the store took the request; the registered listener is the
//! single source of truth for what actually happened, and an `Err` from the
//! request does *not* end the flow (the sheet may already be open), so this
//! repository logs that `Err` and keeps waiting. Settlement is last: Play
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
    Entitlement, PRO_PRODUCT_ID, ProOffer, PurchaseOutcome,
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

// ---------------------------------------------------------------------------
// The store seam
// ---------------------------------------------------------------------------

/// The seven `frust_iap::Iap` calls this repository makes. See the [module
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
        match self.call(|store| store.get_available_purchases(None)).await {
            Ok(rows) => {
                let entitlement = self.entitlement_of(rows).await;
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
        // The one UI-thread-callable call, and the one whose `Err` is not an
        // outcome: the store may already have the sheet open, so only a
        // platform that cannot sell at all ends the attempt here.
        if let Err(err) = self.store.request_purchase(purchase_props())
            && matches!(self.classify(err), StoreGap::Unavailable)
        {
            return Ok(PurchaseOutcome::Unavailable);
        }
        self.await_outcome().await
    }

    async fn restore(&self) -> Result<Entitlement, AppFailure> {
        // Recovered purchases surface through the entitlement query below, not
        // through this call's own return; a failure is logged and the query
        // still runs.
        let _ = self.call(|store| store.restore_purchases()).await;
        self.refresh().await
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
        settled: Vec<String>,
        restore_calls: usize,
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
    }

    impl StoreApi for FakeStore {
        fn init_connection(&self, _config: Option<serde_json::Value>) -> Result<bool, IapError> {
            match self.gap() {
                Some(err) => Err(err),
                None => Ok(true),
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
            _options: Option<PurchaseOptions>,
        ) -> Result<Vec<Purchase>, IapError> {
            match self.gap() {
                Some(err) => Err(err),
                None => Ok(self.state().owned.clone()),
            }
        }

        fn request_purchase(&self, _props: RequestPurchaseProps) -> Result<(), IapError> {
            if let Some(err) = self.gap() {
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
            match self.gap() {
                Some(err) => Err(err),
                None => {
                    self.state().restore_calls += 1;
                    Ok(())
                }
            }
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

    fn repository() -> IapEntitlementRepository<FakeStore> {
        IapEntitlementRepository::with_event_wait(
            FakeStore::new(),
            EntitlementCache::in_memory(),
            TEST_WAIT,
        )
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

    fn refusal(code: IapErrorCode) -> IapEvent {
        IapEvent::PurchaseError(IapPurchaseError {
            code,
            message: "host text that must never be shown".to_string(),
            product_id: Some(PRO_PRODUCT_ID.to_string()),
        })
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
    fn a_restore_re_reads_the_entitlement_after_asking_the_store_to_sync() {
        let repo = repository();
        repo.store.state().owned = vec![row(PurchaseState::Purchased, None)];

        assert_eq!(block_on(repo.restore()), Ok(Entitlement::Pro));
        assert_eq!(repo.store.state().restore_calls, 1);
    }
}
