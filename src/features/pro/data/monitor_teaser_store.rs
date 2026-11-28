//! Durable per-install flag that the free-tier monitor teaser was dismissed: one boolean preferences key.
//! Reads never fail (missing or wrong-typed reads as not dismissed); a failed write is logged, never surfaced.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::spawn_blocking;
use frust_shared_preferences::{PrefsError, SharedPreferences};

use crate::features::pro::domain::repositories::MonitorTeaserStore;

const KEY_DISMISSED: &str = "pro_monitor_teaser_dismissed";

pub(crate) struct PrefsMonitorTeaserStore {
    store: SharedPreferences,
    dismissed_here: AtomicBool,
}

impl PrefsMonitorTeaserStore {
    pub(crate) fn standard() -> Result<Self, PrefsError> {
        Ok(Self {
            store: SharedPreferences::standard()?,
            dismissed_here: AtomicBool::new(false),
        })
    }
}

impl MonitorTeaserStore for PrefsMonitorTeaserStore {
    fn is_dismissed(&self) -> bool {
        if self.dismissed_here.load(Ordering::Relaxed) {
            return true;
        }
        self.store.get_bool(KEY_DISMISSED).unwrap_or(false)
    }

    fn dismiss(&self) {
        self.dismissed_here.store(true, Ordering::Relaxed);
        let store = self.store.clone();
        spawn_blocking(move || {
            if let Err(err) = store.set_bool(KEY_DISMISSED, true) {
                log::warn!("pro monitor teaser: failed to persist the dismissal: {err}");
            }
        });
    }
}

/// Production fallback when the preferences store cannot open: the dismissal holds for this launch only.
#[derive(Clone, Default)]
pub struct InMemoryMonitorTeaserStore {
    dismissed: Arc<AtomicBool>,
}

impl InMemoryMonitorTeaserStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MonitorTeaserStore for InMemoryMonitorTeaserStore {
    fn is_dismissed(&self) -> bool {
        self.dismissed.load(Ordering::Relaxed)
    }

    fn dismiss(&self) {
        self.dismissed.store(true, Ordering::Relaxed);
    }
}

/// Test double: instances built over one backing behave like fresh launches over one preferences store.
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
    fn a_fresh_in_memory_store_is_not_dismissed_until_dismissed() {
        let store = InMemoryMonitorTeaserStore::new();
        assert!(!store.is_dismissed());
        store.dismiss();
        assert!(store.is_dismissed());
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
