//! [`MockSavedServersRepository`] — the in-memory
//! [`SavedServersRepository`] every headless suite runs on, in place of the
//! real, preferences-backed [`super::prefs_saved_servers`] store (whose own
//! module docs explain the split, and `di`'s the seam that chooses).
//!
//! No `frust::` signals/widgets here (the same framework-free rule as `domain`);
//! the store is a plain [`Mutex`]-guarded [`Vec`], not an `RwSignal` — the
//! controller owns the reactive copy and re-reads this one, so the data layer
//! never has to know a UI exists.
//!
//! # Why a `Mutex` and not a `RefCell`
//!
//! `SavedServersRepository` is consumed as
//! `Arc<dyn SavedServersRepository + Send + Sync>` (that trait's docs explain
//! why: `clean_signals_frust::use_controller` requires its controller to be
//! `Send + Sync`, and the controller holds this). muxr is single-UI-threaded, so
//! the lock is never contended — it is a `Send + Sync` obligation, not a
//! concurrency design.
//!
//! # Seed data
//!
//! Three plausible rigs, most-recently-used first, transcribed from
//! `docs/redesign.html`'s `.saved-row` (`100.71.31.57:50051`, "insecure-dev TLS ·
//! port 50051") plus two the same mock's copy implies — a CA-verified public
//! host and a read-only rig, so the row's badge/trust rendering has something to
//! show. Their recency stamps are relative to the seeding call, so the rows
//! read "2 hours ago"/"yesterday"/"last week" whenever a suite runs rather than
//! drifting to a fixed date. A poisoned lock is recovered
//! (`unwrap_or_else(PoisonError::into_inner)`) rather than propagated: a panic
//! on the UI thread has already failed the frame, and handing back a usable
//! guard keeps a later frame renderable.
//!
//! Ordering is not this store's own rule: [`super::prefs_saved_servers::upsert`]
//! is what both implementations sort through, so a `list()` answers in the same
//! order whichever one is wired.

use std::sync::{Mutex, PoisonError};

use super::prefs_saved_servers::upsert;
use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{SavedServer, TrustMode, epoch_millis};
use crate::features::connect::domain::repositories::SavedServersRepository;

/// Milliseconds in an hour — the seed's own recency arithmetic.
const HOUR_MS: i64 = 60 * 60 * 1000;

/// The in-memory saved-server store. See the [module docs](self).
pub struct MockSavedServersRepository {
    servers: Mutex<Vec<SavedServer>>,
}

impl MockSavedServersRepository {
    /// A store holding the three seeded rigs (see the [module docs](self)).
    pub fn seeded() -> Self {
        Self {
            servers: Mutex::new(seed()),
        }
    }

    /// A store holding exactly `servers` — for tests that need a known list.
    pub fn with_servers(servers: Vec<SavedServer>) -> Self {
        Self {
            servers: Mutex::new(servers),
        }
    }
}

/// The seeded rows, most-recently-used first (see the [module docs](self)).
fn seed() -> Vec<SavedServer> {
    let now = epoch_millis();
    vec![
        SavedServer {
            host: "100.71.31.57".to_string(),
            port: 50051,
            trust_mode: TrustMode::TrustOnFirstUse,
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
            TrustMode::TrustOnFirstUse,
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
    fn removing_is_idempotent() {
        let repo = MockSavedServersRepository::with_servers(vec![server("a.example", 1, 10)]);
        repo.remove("a.example:1").expect("remove succeeds");
        repo.remove("a.example:1")
            .expect("removing an absent endpoint still succeeds");
        assert!(repo.list().expect("list succeeds").is_empty());
    }
}
