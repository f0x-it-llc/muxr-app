//! [`MockSavedServersRepository`] — the in-memory
//! [`SavedServersRepository`] the Connect screen runs on until secure storage
//! lands.
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
//! show. A poisoned lock is recovered (`unwrap_or_else(PoisonError::into_inner)`)
//! rather than propagated: a panic on the UI thread has already failed the
//! frame, and handing back a usable guard keeps a later frame renderable.

use std::sync::{Mutex, PoisonError};

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{SavedServer, TrustMode};
use crate::features::connect::domain::repositories::SavedServersRepository;

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

/// The seeded rows, most-recently-used first.
fn seed() -> Vec<SavedServer> {
    vec![
        SavedServer {
            host: "100.71.31.57".to_string(),
            port: 50051,
            trust_mode: TrustMode::TrustOnFirstUse,
            label: "Home rig".to_string(),
            last_used: "2 hours ago".to_string(),
            read_only: false,
        },
        SavedServer {
            host: "zelli.example.com".to_string(),
            port: 50051,
            trust_mode: TrustMode::System,
            label: "zelli".to_string(),
            last_used: "yesterday".to_string(),
            read_only: false,
        },
        SavedServer {
            host: "herdr.internal".to_string(),
            port: 7443,
            trust_mode: TrustMode::Pinned,
            label: "herdr staging".to_string(),
            last_used: "last week".to_string(),
            read_only: true,
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
        let endpoint = server.endpoint();
        // Most-recently-used first: a re-saved endpoint moves to the top rather
        // than staying wherever it was.
        servers.retain(|existing| existing.endpoint() != endpoint);
        servers.insert(0, server);
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

    fn server(host: &str, port: u16) -> SavedServer {
        SavedServer {
            host: host.to_string(),
            port,
            trust_mode: TrustMode::System,
            label: host.to_string(),
            last_used: "just now".to_string(),
            read_only: false,
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
    fn saving_a_new_endpoint_puts_it_first() {
        let repo = MockSavedServersRepository::with_servers(vec![server("a.example", 1)]);
        repo.save(server("b.example", 2)).expect("save succeeds");

        let listed = repo.list().expect("list succeeds");
        assert_eq!(listed[0].endpoint(), "b.example:2");
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn re_saving_an_endpoint_replaces_it_rather_than_duplicating() {
        let repo = MockSavedServersRepository::with_servers(vec![
            server("a.example", 1),
            server("b.example", 2),
        ]);
        let mut updated = server("b.example", 2);
        updated.label = "renamed".to_string();
        repo.save(updated).expect("save succeeds");

        let listed = repo.list().expect("list succeeds");
        assert_eq!(listed.len(), 2, "no duplicate row");
        assert_eq!(listed[0].label, "renamed", "…and it moved to the top");
    }

    #[test]
    fn removing_is_idempotent() {
        let repo = MockSavedServersRepository::with_servers(vec![server("a.example", 1)]);
        repo.remove("a.example:1").expect("remove succeeds");
        repo.remove("a.example:1")
            .expect("removing an absent endpoint still succeeds");
        assert!(repo.list().expect("list succeeds").is_empty());
    }
}
