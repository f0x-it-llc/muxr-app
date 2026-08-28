//! [`EntitlementCache`] — the durable half of the store-backed entitlement
//! repository: two `frust-shared-preferences` keys holding the last answer the
//! store gave, so a cold start and an offline launch can gate on something
//! other than Free-until-proven.
//!
//! **Reads never fail**: a missing or wrong-typed value reads as
//! [`Entitlement::Free`], the safe side of a gate. [`Entitlement::Pending`] is
//! deliberately not representable on disk — an unsettled purchase unlocks
//! nothing — so it stores, and reads back, as Free.
//!
//! **Writes are fire-and-forget**, the shape `settings`' `PrefsRepository`
//! savers use: a failed write is logged rather than surfaced, since losing the
//! cache costs one store round trip on the next launch, never the entitlement.

use frust::spawn_blocking;
use frust_shared_preferences::{PrefsError, SharedPreferences};

use crate::features::connect::domain::entities::epoch_millis;
use crate::features::pro::domain::entities::Entitlement;

/// Whether the last store answer entitled this install to Pro.
const KEY_ENTITLEMENT: &str = "pro_entitlement";
/// When that answer was written, in ms since the Unix epoch.
const KEY_CHECKED_MS: &str = "pro_entitlement_checked_ms";

/// The persisted entitlement. See the [module docs](self).
pub(crate) struct EntitlementCache {
    backend: Backend,
}

/// Where the two keys live.
enum Backend {
    /// The platform's standard preferences store — the only arm a real run has.
    Prefs(SharedPreferences),
    /// An in-process map the unit tests run on. A headless test process has
    /// neither a platform store nor the reactive runtime [`spawn_blocking`]
    /// needs, so the real arm cannot be exercised there at all.
    #[cfg(test)]
    Memory(std::sync::Mutex<Option<bool>>),
}

impl EntitlementCache {
    /// Open the platform's standard preferences store.
    ///
    /// # Errors
    /// See [`SharedPreferences::standard`]'s own contract.
    pub(crate) fn standard() -> Result<Self, PrefsError> {
        Ok(Self {
            backend: Backend::Prefs(SharedPreferences::standard()?),
        })
    }

    /// The last persisted answer, synchronously — what a cold start gates on.
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

    /// Overwrite the cache with `entitlement` and stamp the check time.
    /// Overwriting rather than merging is what lets a refund propagate back to
    /// Free on the next refresh.
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

    /// An empty in-process cache, for the repository's own unit tests.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            backend: Backend::Memory(std::sync::Mutex::new(None)),
        }
    }
}

/// The stored flag's meaning: only an explicit `true` is Pro. A key that was
/// never written, or holds something that isn't a bool, reads as Free.
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
