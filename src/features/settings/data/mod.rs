//! `settings` data layer — implements this feature's `domain` repository
//! trait(s) over a real source (local preference storage, …). No `frust::`
//! reactive/widget types here — the data layer stays as framework-free as
//! `domain` (see `crate::features::settings::domain`'s module doc), differing
//! only in that it may depend on an external transport/storage crate. Empty
//! until Phase 2 wires a real implementation.

pub mod repositories;
