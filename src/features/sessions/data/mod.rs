//! `sessions` data layer — implements this feature's `domain` repository
//! trait(s) over a real source (a local session-list store, a network
//! discovery client, …). No `frust::` reactive/widget types here — the data
//! layer stays as framework-free as `domain` (see
//! `crate::features::sessions::domain`'s module doc), differing only in that
//! it may depend on an external transport/storage crate. Empty until Phase 2
//! wires a real implementation.

pub mod repositories;
