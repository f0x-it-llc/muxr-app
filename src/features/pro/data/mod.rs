//! `pro` data layer — implements this feature's `domain` repository contract
//! over a concrete source. Framework-free like `domain`, differing only in
//! that it may depend on an external store/platform crate.

pub mod entitlement_cache;
pub mod repositories;
