//! `settings` data layer — implements this feature's two `domain` repository
//! traits over concrete sources. No `frust::` reactive/widget types here — the
//! data layer stays as framework-free as `domain` (see
//! `crate::features::settings::domain`'s module doc), differing only in that it
//! may depend on an external transport/storage crate.
//!
//! Today both sources are in-memory
//! ([`repositories::MockAuthTokensRepository`],
//! [`repositories::MockPreferencesRepository`]) — there is no `muxrd` gRPC
//! round trip (`crate::connection`'s module docs) and no storage plugin
//! (`docs/ARCHITECTURE.md`'s Planned section) yet. They are *real*
//! implementations of the real contracts, not stubs: each mutates its own
//! store, fails with the app's own failure vocabulary, and awaits a small
//! simulated latency, so everything above them already runs the code path the
//! real sources will.

pub mod repositories;
