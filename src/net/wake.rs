//! The app-scoped wake signal: a stalled connection is worth retrying now. Consumers never
//! learn where a wake came from; `MuxrApp::init` installs the production source.

use std::sync::{Arc, OnceLock};

use tokio::sync::broadcast;

/// Buffered wakes per subscriber before the oldest are dropped.
const WAKE_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeReason {
    NetworkChanged,
    Foregrounded,
}

/// Publishing never blocks and needs no runtime; a lagging subscriber loses its oldest wakes.
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

    pub fn subscribe(&self) -> broadcast::Receiver<WakeReason> {
        self.tx.subscribe()
    }

    pub fn publish(&self, reason: WakeReason) {
        let _ = self.tx.send(reason);
    }
}

impl Default for WakeBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Installed once; publishes from whichever thread its events arrive on.
pub trait WakeSource: Send + Sync + 'static {
    fn start(&self, publisher: WakePublisher);
}

#[derive(Clone, Debug)]
pub struct WakePublisher {
    bus: WakeBus,
}

impl WakePublisher {
    pub fn publish(&self, reason: WakeReason) {
        self.bus.publish(reason);
    }
}

static APP_BUS: OnceLock<WakeBus> = OnceLock::new();
static SOURCE: OnceLock<Arc<dyn WakeSource>> = OnceLock::new();

fn app_bus() -> &'static WakeBus {
    APP_BUS.get_or_init(WakeBus::new)
}

pub fn subscribe() -> broadcast::Receiver<WakeReason> {
    app_bus().subscribe()
}

/// `false`, with `source` dropped unstarted, when one is already installed.
pub fn install_wake_source(source: Arc<dyn WakeSource>) -> bool {
    if SOURCE.set(source.clone()).is_err() {
        return false;
    }
    source.start(WakePublisher {
        bus: app_bus().clone(),
    });
    true
}

/// `restart` is `Hidden` reached from `Paused`: the notification shade, Control Center and
/// the app switcher only pass `Inactive` and never fire it.
pub fn wake_for_restart() -> WakeReason {
    WakeReason::Foregrounded
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkSignal {
    /// `initial` marks the monitor's first report.
    Available {
        initial: bool,
    },
    Lost,
    Other,
}

/// Only a later `Available` wakes; the monitor's first report and `Lost` do not.
pub fn wake_for_network(signal: NetworkSignal) -> Option<WakeReason> {
    match signal {
        NetworkSignal::Available { initial: false } => Some(WakeReason::NetworkChanged),
        _ => None,
    }
}

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

    // `Available` events cannot be built outside frust-connectivity, so that mapping is tested
    // through `NetworkSignal`.
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
