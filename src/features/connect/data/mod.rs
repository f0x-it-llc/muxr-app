//! `connect` data layer — implements this feature's `domain` repository contracts
//! over a real source. No `frust::` reactive/widget types here: the data layer
//! stays as framework-free as `domain`, differing only in that it may depend on an
//! external transport/storage crate.
//!
//! Two contracts, two implementations each — a real one and the in-memory double
//! every headless suite runs on. See [`repositories`]' own table.

pub mod repositories;
