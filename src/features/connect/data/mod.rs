//! `connect` data layer — implements this feature's `domain` repository
//! trait(s) over a real source (a network client, local credential store, …).
//! No `frust::` reactive/widget types here — the data layer stays as
//! framework-free as `domain` (see `crate::features::connect::domain`'s
//! module doc), differing only in that it may depend on an external
//! transport/storage crate.
//!
//! Two contracts, two implementations each — a real one and the in-memory
//! double every headless suite runs on. See [`repositories`]' own table.

pub mod repositories;
