//! `terminal` data layer — implements this feature's `domain` repository
//! trait(s) over a real source (a PTY/socket transport, …). No `frust::`
//! reactive/widget types here — the data layer stays as framework-free as
//! `domain` (see `crate::features::terminal::domain`'s module doc),
//! differing only in that it may depend on an external transport/storage
//! crate.
//!
//! Two implementations today, one per `domain` port:
//! `repositories::MockTerminalRepository` (the screen's model, seeded from
//! `docs/terminal.html`) and `feeds::DemoFeed` (the grid's byte stream,
//! replaying a checked-in fixture). The real `muxrd` client lands beside both.

pub mod feeds;
pub mod repositories;
