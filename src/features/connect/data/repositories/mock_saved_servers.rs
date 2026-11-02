//! [`MockSavedServersRepository`] — the in-memory
//! [`SavedServersRepository`] every headless suite runs on, in place of the
//! real, preferences-backed [`super::prefs_saved_servers`] store.
//!
//! The store is a plain [`Mutex`]-guarded [`Vec`], not an `RwSignal` — the
//! controller owns the reactive copy and re-reads this one. The lock is a
//! `Send + Sync` obligation, not a concurrency design (muxr is
//! single-UI-threaded and it's never contended). A poisoned lock is recovered
//! rather than propagated — a panic on the UI thread has already failed the
//! frame, and a usable guard keeps a later frame renderable.
//!
//! Seed data: three plausible rigs, most-recently-used first, covering every
//! [`TrustMode`] and one read-only row so badge/trust rendering has something
//! to show. Recency stamps are relative to the seeding call, so rows read
//! "2 hours ago"/etc. whenever a suite runs rather than drifting to a fixed date.
//!
//! Ordering isn't this store's own rule: [`super::prefs_saved_servers::upsert`]
//! is what both implementations sort through.
//!
//! It answers [`SavedServersRepository::is_persistent`] `true`, standing in
//! for the real store; [`MockSavedServersRepository::not_persistent`] marks
//! the production fallback's copy, whose demo rows must never drive a prune.

use std::sync::{Mutex, PoisonError};

use super::prefs_saved_servers::upsert;
use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{SavedServer, TrustMode, epoch_millis};
use crate::features::connect::domain::repositories::SavedServersRepository;

const HOUR_MS: i64 = 60 * 60 * 1000;

/// The in-memory saved-server store. See the [module docs](self).
pub struct MockSavedServersRepository {
    servers: Mutex<Vec<SavedServer>>,
    persistent: bool,
}

impl MockSavedServersRepository {
    /// See the [module docs](self) for the seeded rigs.
    pub fn seeded() -> Self {
        Self::with_servers(seed())
    }

    /// A store holding exactly `servers` — for tests that need a known list.
    pub fn with_servers(servers: Vec<SavedServer>) -> Self {
        Self {
            servers: Mutex::new(servers),
            persistent: true,
        }
    }

    /// This store, answering [`SavedServersRepository::is_persistent`]
    /// `false`.
    pub fn not_persistent(mut self) -> Self {
        self.persistent = false;
        self
    }
}

fn seed() -> Vec<SavedServer> {
    let now = epoch_millis();
    vec![
        SavedServer {
            host: "100.71.31.57".to_string(),
            port: 50051,
            trust_mode: TrustMode::NoVerification,
            label: "Home rig".to_string(),
            last_used_ms: now - 2 * HOUR_MS,
            read_only: false,
            has_credential: false,
            token_name: None,
            fingerprint: None,
        },
        SavedServer {
            host: "zelli.example.com".to_string(),
            port: 50051,
            trust_mode: TrustMode::System,
            label: "zelli".to_string(),
            last_used_ms: now - 30 * HOUR_MS,
            read_only: false,
            has_credential: false,
            token_name: None,
            fingerprint: None,
        },
        SavedServer {
            host: "herdr.internal".to_string(),
            port: 7443,
            trust_mode: TrustMode::Pinned,
            label: "herdr staging".to_string(),
            last_used_ms: now - 9 * 24 * HOUR_MS,
            read_only: true,
            has_credential: false,
            token_name: None,
            fingerprint: Some("ab".repeat(32)),
        },
    ]
}

impl SavedServersRepository for MockSavedServersRepository {
    fn list(&self) -> Result<Vec<SavedServer>, AppFailure> {
        let servers = self.servers.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(servers.clone())
    }

    fn save(&self, server: SavedServer) -> Result<(), AppFailure> {
        let mut servers = self.servers.lock().unwrap_or_else(PoisonError::into_inner);
        upsert(&mut servers, server);
        Ok(())
    }

    fn remove(&self, endpoint: &str) -> Result<(), AppFailure> {
        let mut servers = self.servers.lock().unwrap_or_else(PoisonError::into_inner);
        servers.retain(|existing| existing.endpoint() != endpoint);
        Ok(())
    }

    fn is_persistent(&self) -> bool {
        self.persistent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, port: u16, last_used_ms: i64) -> SavedServer {
        SavedServer {
            host: host.to_string(),
            port,
            trust_mode: TrustMode::System,
            label: host.to_string(),
            last_used_ms,
            read_only: false,
            has_credential: false,
            token_name: None,
            fingerprint: None,
        }
    }

    #[test]
    fn the_seed_is_three_distinct_endpoints() {
        let repo = MockSavedServersRepository::seeded();
        let listed = repo.list().expect("the mock store never fails");
        assert_eq!(listed.len(), 3);
        let mut endpoints: Vec<String> = listed.iter().map(SavedServer::endpoint).collect();
        endpoints.sort();
        endpoints.dedup();
        assert_eq!(endpoints.len(), 3, "every seeded row is its own endpoint");
    }

    #[test]
    fn the_seed_covers_every_trust_mode_and_one_read_only_row() {
        let repo = MockSavedServersRepository::seeded();
        let listed = repo.list().expect("the mock store never fails");
        for mode in [
            TrustMode::System,
            TrustMode::NoVerification,
            TrustMode::Pinned,
        ] {
            assert!(
                listed.iter().any(|s| s.trust_mode == mode),
                "the seed must exercise {mode:?} so the row chrome has something to render"
            );
        }
        assert_eq!(listed.iter().filter(|s| s.read_only).count(), 1);
    }

    #[test]
    fn saving_a_more_recent_endpoint_puts_it_first() {
        let repo = MockSavedServersRepository::with_servers(vec![server("a.example", 1, 10)]);
        repo.save(server("b.example", 2, 20))
            .expect("save succeeds");

        let listed = repo.list().expect("list succeeds");
        assert_eq!(listed[0].endpoint(), "b.example:2");
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn re_saving_an_endpoint_replaces_it_rather_than_duplicating() {
        let repo = MockSavedServersRepository::with_servers(vec![
            server("a.example", 1, 10),
            server("b.example", 2, 20),
        ]);
        let mut updated = server("b.example", 2, 30);
        updated.label = "renamed".to_string();
        repo.save(updated).expect("save succeeds");

        let listed = repo.list().expect("list succeeds");
        assert_eq!(listed.len(), 2, "no duplicate row");
        assert_eq!(listed[0].label, "renamed", "…and it moved to the top");
    }

    #[test]
    fn the_mock_stands_in_for_a_persistent_store_unless_marked_otherwise() {
        assert!(MockSavedServersRepository::seeded().is_persistent());
        assert!(
            !MockSavedServersRepository::seeded()
                .not_persistent()
                .is_persistent()
        );
    }

    #[test]
    fn removing_is_idempotent() {
        let repo = MockSavedServersRepository::with_servers(vec![server("a.example", 1, 10)]);
        repo.remove("a.example:1").expect("remove succeeds");
        repo.remove("a.example:1")
            .expect("removing an absent endpoint still succeeds");
        assert!(repo.list().expect("list succeeds").is_empty());
    }
}
