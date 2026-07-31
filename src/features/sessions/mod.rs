//! The `sessions` feature slice — muxr's `/sessions` route: the list of a
//! connected server's terminal sessions, with create/rename/kill (a card tap
//! attaches, a long-press opens the management sheet those two live in), the
//! fixed server-info + latency strip, the save-credential offer, and the attach
//! path into `/terminal`.
//!
//! `{domain, data, presentation}`, per the clean-architecture convention
//! `examples/huddle` establishes (see its `src/README-phase-c.md`):
//! dependencies point inward only, `presentation` → `domain` ← `data`. [`di`]
//! is the composition-root wiring seam `crate::MuxrApp::init` calls into (see
//! its module docs) — not part of the inward-dependency slice itself, and the
//! only place the three meet.
//!
//! **Mock-backed**: there is no `muxrd` gRPC round trip yet (see
//! `crate::connection`'s module docs), so `data`'s repository is in-memory.
//! Everything above it — the async repository contract, the use cases, the
//! controller's `ControllerCore::run` calls, the screen's spawn/await/refresh
//! path — is already the code the real client will run.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
