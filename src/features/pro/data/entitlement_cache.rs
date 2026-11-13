//! Durable half of the store-backed entitlement: the last store answer in two preferences keys.
//! Reads never fail: missing or wrong-typed reads as Free, and Pending is stored as Free.
//! Writes are fire-and-forget: a failed write is logged, never surfaced.

use frust::spawn_blocking;
use frust_shared_preferences::{PrefsError, SharedPreferences};

use crate::features::connect::domain::entities::epoch_millis;
use crate::features::pro::domain::entities::Entitlement;

const KEY_ENTITLEMENT: &str = "pro_entitlement";
const KEY_CHECKED_MS: &str = "pro_entitlement_checked_ms";

pub(crate) struct EntitlementCache {
    backend: Backend,
}

enum Backend {
    Prefs(SharedPreferences),
    /// In-process map for unit tests: a headless process has no platform store or reactive runtime.
    #[cfg(test)]
    Memory(std::sync::Mutex<Option<bool>>),
}

impl EntitlementCache {
    pub(crate) fn standard() -> Result<Self, PrefsError> {
        Ok(Self {
            backend: Backend::Prefs(SharedPreferences::standard()?),
        })
    }

    pub(crate) fn read(&self) -> Entitlement {
        let stored = match &self.backend {
            Backend::Prefs(store) => store.get_bool(KEY_ENTITLEMENT),
            #[cfg(test)]
            Backend::Memory(cell) => *cell
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        };
        entitlement_from_flag(stored)
    }

    /// Overwrites rather than merges so a refund propagates back to Free.
    pub(crate) fn write(&self, entitlement: Entitlement) {
        let is_pro = entitlement.is_pro();
        match &self.backend {
            Backend::Prefs(store) => {
                let store = store.clone();
                spawn_blocking(move || {
                    if let Err(err) = store.set_bool(KEY_ENTITLEMENT, is_pro) {
                        log::warn!("pro entitlement: failed to persist the entitlement: {err}");
                    }
                    if let Err(err) = store.set_i64(KEY_CHECKED_MS, epoch_millis()) {
                        log::warn!("pro entitlement: failed to persist the check time: {err}");
                    }
                });
            }
            #[cfg(test)]
            Backend::Memory(cell) => {
                *cell
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(is_pro);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            backend: Backend::Memory(std::sync::Mutex::new(None)),
        }
    }
}

/// Only an explicit `true` is Pro.
fn entitlement_from_flag(stored: Option<bool>) -> Entitlement {
    match stored {
        Some(true) => Entitlement::Pro,
        _ => Entitlement::Free,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unwritten_or_corrupt_key_reads_as_free() {
        assert_eq!(entitlement_from_flag(None), Entitlement::Free);
        assert_eq!(entitlement_from_flag(Some(false)), Entitlement::Free);
    }

    #[test]
    fn only_a_stored_true_reads_as_pro() {
        assert_eq!(entitlement_from_flag(Some(true)), Entitlement::Pro);
    }

    #[test]
    fn a_write_round_trips_pro_and_a_later_free_overwrites_it() {
        let cache = EntitlementCache::in_memory();
        assert_eq!(cache.read(), Entitlement::Free);

        cache.write(Entitlement::Pro);
        assert_eq!(cache.read(), Entitlement::Pro);

        cache.write(Entitlement::Free);
        assert_eq!(cache.read(), Entitlement::Free);
    }

    #[test]
    fn a_pending_purchase_is_cached_as_free() {
        let cache = EntitlementCache::in_memory();
        cache.write(Entitlement::Pro);
        cache.write(Entitlement::Pending);
        assert_eq!(cache.read(), Entitlement::Free);
    }
}
