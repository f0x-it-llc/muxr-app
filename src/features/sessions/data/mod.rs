//! `sessions` data layer — implements this feature's `domain` repository
//! trait over a concrete source. No `frust::` reactive/widget types here — the
//! data layer stays as framework-free as `domain` (see
//! `crate::features::sessions::domain`'s module doc), differing only in that
//! it may depend on an external transport/storage crate.
//!
//! Today that source is in-memory
//! ([`repositories::MockSessionsRepository`]) — there is no `muxrd` gRPC round
//! trip yet (`crate::connection`'s module docs). It is a *real* implementation
//! of the real contract, not a stub: it mutates its own store, fails with the
//! app's own failure vocabulary, and awaits a small simulated latency, so
//! everything above it already runs the code path the real client will.

pub mod repositories;
