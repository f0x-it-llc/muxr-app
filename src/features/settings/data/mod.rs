//! `settings` data layer — implements this feature's two `domain` repository
//! traits over concrete sources. No `frust::` reactive/widget types here —
//! framework-free like `domain`, differing only in that it may depend on an
//! external transport/storage crate. The real sources back a production run
//! (`GrpcAuthTokensRepository` over `muxrd`, `PrefsRepository` over
//! `frust-shared-preferences`); the in-memory mocks stay behind `di.rs`'s
//! test-only seam for every headless suite.

pub mod repositories;
