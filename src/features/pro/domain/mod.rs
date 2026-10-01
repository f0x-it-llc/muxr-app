//! `pro` domain — entities, the store contract, and use cases.
//! Framework-free: never imports `frust::` or a `data`/`presentation` path
//! segment.
//!
//! Deviation: [`entities`] reads `connect`'s own `SavedServer` rather than
//! minting a second saved-server type — see that module's docs.

pub mod entities;
pub mod repositories;
pub mod use_cases;
