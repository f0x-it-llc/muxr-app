//! Durable per-install flag that the free-tier monitor teaser was dismissed: one boolean preferences key.
//! Reads never fail (missing or wrong-typed reads as not dismissed); a failed write is logged, never surfaced.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::spawn_blocking;
use frust_shared_preferences::{PrefsError, SharedPreferences};

use crate::features::pro::domain::repositories::MonitorTeaserStore;

const KEY_DISMISSED: &str = "pro_monitor_teaser_dismissed";

pub(crate) struct PrefsMonitorTeaserStore {
    backend: Backend,
    dismissed_here: AtomicBool,
}

enum Backend {
    Prefs(SharedPreferences),
    /// In-process flag for unit tests: a headless process has no platform store or reactive runtime.
    #[cfg(test)]
    Memory(Arc<AtomicBool>),
}

impl PrefsMonitorTeaserStore {
    pub(crate) fn standard() -> Result<Self, PrefsError> {
        Ok(Self::over(Backend::Prefs(SharedPreferences::standard()?)))
    }

    fn over(backend: Backend) -> Self {
        Self {
            backend,
            dismissed_here: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    fn in_memory() -> Self {
        Self::over(Backend::Memory(Arc::new(AtomicBool::new(false))))
    }

    #[cfg(test)]
    fn sharing_backing_with(other: &Self) -> Self {
        match &other.backend {
            Backend::Memory(flag) => Self::over(Backend::Memory(Arc::clone(flag))),
            Backend::Prefs(_) => unreachable!("only the in-memory backend is shared"),
        }
    }
}

impl MonitorTeaserStore for PrefsMonitorTeaserStore {
    fn is_dismissed(&self) -> bool {
        if self.dismissed_here.load(Ordering::Relaxed) {
            return true;
        }
        match &self.backend {
            Backend::Prefs(store) => store.get_bool(KEY_DISMISSED).unwrap_or(false),
            #[cfg(test)]
            Backend::Memory(flag) => flag.load(Ordering::Relaxed),
        }
    }

    fn dismiss(&self) {
        self.dismissed_here.store(true, Ordering::Relaxed);
        match &self.backend {
            Backend::Prefs(store) => {
                let store = store.clone();
                spawn_blocking(move || {
                    if let Err(err) = store.set_bool(KEY_DISMISSED, true) {
                        log::warn!("pro monitor teaser: failed to persist the dismissal: {err}");
                    }
                });
            }
            #[cfg(test)]
            Backend::Memory(flag) => flag.store(true, Ordering::Relaxed),
        }
    }
}

/// Headless stand-in; instances built over one backing behave like fresh launches over one preferences store.
#[derive(Clone, Default)]
pub struct MockMonitorTeaserStore {
    dismissed: Arc<AtomicBool>,
}

impl MockMonitorTeaserStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sharing_backing_with(other: &Self) -> Self {
        Self {
            dismissed: Arc::clone(&other.dismissed),
        }
    }
}

impl MonitorTeaserStore for MockMonitorTeaserStore {
    fn is_dismissed(&self) -> bool {
        self.dismissed.load(Ordering::Relaxed)
    }

    fn dismiss(&self) {
        self.dismissed.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_store_is_not_dismissed() {
        assert!(!PrefsMonitorTeaserStore::in_memory().is_dismissed());
    }

    #[test]
    fn a_dismissal_survives_into_a_fresh_instance_over_the_same_backing() {
        let first = PrefsMonitorTeaserStore::in_memory();
        first.dismiss();
        assert!(first.is_dismissed());

        let relaunched = PrefsMonitorTeaserStore::sharing_backing_with(&first);
        assert!(relaunched.is_dismissed());
    }

    #[test]
    fn the_mock_shares_a_dismissal_across_instances_over_one_backing() {
        let first = MockMonitorTeaserStore::new();
        let second = MockMonitorTeaserStore::sharing_backing_with(&first);
        assert!(!second.is_dismissed());
        first.dismiss();
        assert!(second.is_dismissed());
    }
}
