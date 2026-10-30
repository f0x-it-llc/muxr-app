//! The app-scoped wake signal: "something happened that makes a stalled
//! connection worth retrying now". Consumers (the terminal feed, the Sessions
//! heartbeat) subscribe and react; they never learn where a wake came from.
//!
//! The production source is installed by `MuxrApp::init` (see `src/lib.rs`):
//! its `FrustWakeSource` publishes from frust's connectivity events (a changed
//! network path) and lifecycle events (a return to the foreground), so a
//! subscriber sees wakes whenever the platform reports either. Free of feature
//! types: features depend on `net`, never the reverse.

use std::sync::{Arc, OnceLock};

use tokio::sync::broadcast;

/// Buffered wakes per subscriber before the oldest are dropped.
const WAKE_CAPACITY: usize = 16;

/// Why the app is asking connections to re-check themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeReason {
    /// The OS reported a changed network path.
    NetworkChanged,
    /// The app returned to the foreground.
    Foregrounded,
}

/// A broadcast of [`WakeReason`]s. Publishing never blocks and needs no
/// runtime; a subscriber that falls behind loses its oldest wakes.
#[derive(Clone, Debug)]
pub struct WakeBus {
    tx: broadcast::Sender<WakeReason>,
}

impl WakeBus {
    pub fn new() -> Self {
        Self {
            tx: broadcast::channel(WAKE_CAPACITY).0,
        }
    }

    /// Receive every wake published after this call.
    pub fn subscribe(&self) -> broadcast::Receiver<WakeReason> {
        self.tx.subscribe()
    }

    /// Publish a wake from any thread. Never blocks; with no subscriber the
    /// wake is simply discarded.
    pub fn publish(&self, reason: WakeReason) {
        let _ = self.tx.send(reason);
    }
}

impl Default for WakeBus {
    fn default() -> Self {
        Self::new()
    }
}

/// A platform source of wakes. Installed once at app init via
/// [`install_wake_source`]; it publishes through the handed-in [`WakePublisher`]
/// from whichever thread its events arrive on.
pub trait WakeSource: Send + Sync + 'static {
    fn start(&self, publisher: WakePublisher);
}

/// A cheap, cloneable, thread-safe handle a [`WakeSource`] publishes through.
#[derive(Clone, Debug)]
pub struct WakePublisher {
    bus: WakeBus,
}

impl WakePublisher {
    /// Publish a wake. Callable from any thread; never blocks.
    pub fn publish(&self, reason: WakeReason) {
        self.bus.publish(reason);
    }
}

static APP_BUS: OnceLock<WakeBus> = OnceLock::new();
static SOURCE: OnceLock<Arc<dyn WakeSource>> = OnceLock::new();

fn app_bus() -> &'static WakeBus {
    APP_BUS.get_or_init(WakeBus::new)
}

/// Subscribe to the app-scoped wake signal.
pub fn subscribe() -> broadcast::Receiver<WakeReason> {
    app_bus().subscribe()
}

/// Install the app's one wake source and start it. Returns `false` (and drops
/// `source` unstarted) when a source was already installed.
pub fn install_wake_source(source: Arc<dyn WakeSource>) -> bool {
    if SOURCE.set(source.clone()).is_err() {
        return false;
    }
    source.start(WakePublisher {
        bus: app_bus().clone(),
    });
    true
}

/// The wake for the lifecycle `restart` transition: `Hidden` reached from
/// `Paused`, i.e. a return from a real background. The notification shade,
/// Control Center and the app switcher only pass `Inactive` and never fire it.
pub fn wake_for_restart() -> WakeReason {
    WakeReason::Foregrounded
}

/// What a default-network report says, stripped of the path details the wake
/// decision ignores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkSignal {
    /// A usable default network; `initial` marks the monitor's first report.
    Available { initial: bool },
    /// The usable default network went away.
    Lost,
    /// Any event this app does not act on.
    Other,
}

/// The wake, if any, for a network signal. Only a later `Available` (the
/// default network changed) wakes; the monitor's first report and `Lost` do
/// not.
pub fn wake_for_network(signal: NetworkSignal) -> Option<WakeReason> {
    match signal {
        NetworkSignal::Available { initial: false } => Some(WakeReason::NetworkChanged),
        _ => None,
    }
}

/// The wake, if any, for a connectivity event. The event enum is
/// non-exhaustive, so unknown events map to [`NetworkSignal::Other`].
pub fn wake_for_connectivity(event: &frust_connectivity::ConnectivityEvent) -> Option<WakeReason> {
    use frust_connectivity::ConnectivityEvent;
    wake_for_network(match event {
        ConnectivityEvent::Available { initial, .. } => {
            NetworkSignal::Available { initial: *initial }
        }
        ConnectivityEvent::Lost => NetworkSignal::Lost,
        _ => NetworkSignal::Other,
    })
}

/// Publish a wake on the app bus as if a source had. Tests only.
#[cfg(test)]
pub fn publish_for_tests(reason: WakeReason) {
    app_bus().publish(reason);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::TryRecvError;

    #[test]
    fn two_subscribers_both_receive_a_published_wake() {
        let bus = WakeBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.publish(WakeReason::NetworkChanged);
        assert_eq!(a.try_recv(), Ok(WakeReason::NetworkChanged));
        assert_eq!(b.try_recv(), Ok(WakeReason::NetworkChanged));
    }

    #[test]
    fn a_subscriber_that_never_polls_does_not_block_publishing() {
        let bus = WakeBus::new();
        let mut idle = bus.subscribe();
        let mut live = bus.subscribe();
        for _ in 0..(WAKE_CAPACITY * 4) {
            bus.publish(WakeReason::Foregrounded);
            assert_eq!(live.try_recv(), Ok(WakeReason::Foregrounded));
        }
        assert!(matches!(idle.try_recv(), Err(TryRecvError::Lagged(_))));
        assert_eq!(idle.try_recv(), Ok(WakeReason::Foregrounded));
    }

    #[test]
    fn nothing_is_published_when_no_source_is_installed() {
        let bus = WakeBus::new();
        let mut rx = bus.subscribe();
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn publishing_from_a_plain_thread_needs_no_runtime() {
        let bus = WakeBus::new();
        let mut rx = bus.subscribe();
        let publisher = WakePublisher { bus: bus.clone() };
        std::thread::spawn(move || publisher.publish(WakeReason::Foregrounded))
            .join()
            .unwrap();
        assert_eq!(rx.try_recv(), Ok(WakeReason::Foregrounded));
    }

    #[test]
    fn a_source_publishes_through_the_handed_in_publisher() {
        struct Fire;
        impl WakeSource for Fire {
            fn start(&self, publisher: WakePublisher) {
                std::thread::spawn(move || publisher.publish(WakeReason::NetworkChanged))
                    .join()
                    .unwrap();
            }
        }
        let mut rx = subscribe();
        assert!(install_wake_source(Arc::new(Fire)));
        assert_eq!(rx.try_recv(), Ok(WakeReason::NetworkChanged));
        assert!(!install_wake_source(Arc::new(Fire)));
        publish_for_tests(WakeReason::Foregrounded);
        assert_eq!(rx.try_recv(), Ok(WakeReason::Foregrounded));
    }

    // `NetworkPath` is non-exhaustive with crate-private constructors, so an
    // `Available` event cannot be built outside frust-connectivity: its
    // mapping is tested through `NetworkSignal`, `Lost` through the event.
    #[test]
    fn the_first_connectivity_report_does_not_wake() {
        assert_eq!(
            wake_for_network(NetworkSignal::Available { initial: true }),
            None
        );
    }

    #[test]
    fn a_later_available_wakes_as_a_network_change() {
        assert_eq!(
            wake_for_network(NetworkSignal::Available { initial: false }),
            Some(WakeReason::NetworkChanged)
        );
    }

    #[test]
    fn a_lost_network_does_not_wake() {
        assert_eq!(wake_for_network(NetworkSignal::Lost), None);
        assert_eq!(wake_for_network(NetworkSignal::Other), None);
        assert_eq!(
            wake_for_connectivity(&frust_connectivity::ConnectivityEvent::Lost),
            None
        );
    }

    #[test]
    fn a_lifecycle_restart_wakes_as_foregrounded() {
        assert_eq!(wake_for_restart(), WakeReason::Foregrounded);
    }
}
