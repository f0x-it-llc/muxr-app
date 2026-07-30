//! `terminal` data layer — implements this feature's `domain` repository
//! trait(s) over a real source (a PTY/socket transport, …). No `frust::`
//! reactive/widget types here — the data layer stays as framework-free as
//! `domain` (see `crate::features::terminal::domain`'s module doc),
//! differing only in that it may depend on an external transport/storage
//! crate. Empty until Phase 2 wires a real implementation.

pub mod repositories;
