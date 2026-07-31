//! `connect` data layer — implements this feature's `domain` repository
//! trait(s) over a real source (a network client, local credential store, …).
//! No `frust::` reactive/widget types here — the data layer stays as
//! framework-free as `domain` (see `crate::features::connect::domain`'s
//! module doc), differing only in that it may depend on an external
//! transport/storage crate.
//!
//! One implementation today: [`repositories::mock_saved_servers`], an in-memory
//! saved-server store. Nothing persists yet — secure credential storage is a
//! later phase (`docs/ARCHITECTURE.md`'s Planned section), and the mock is what
//! lets the Connect screen be real before it exists.

pub mod repositories;
