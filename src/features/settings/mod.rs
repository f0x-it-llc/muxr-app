//! The `settings` feature slice — muxr's `/settings` route: the connected
//! server's own settings screen. Four groups (`connection`, `auth tokens`,
//! `terminal appearance`, `session poll`) plus a Disconnect action, with the
//! reusable picker sheet the last two groups' rows open.
//!
//! `{domain, data, presentation}`, per the clean-architecture convention
//! `examples/huddle` establishes (see its `src/README-phase-c.md`):
//! dependencies point inward only, `presentation` → `domain` ← `data`. [`di`]
//! is the composition-root wiring seam `crate::MuxrApp::init` calls into (see
//! its module docs) — not part of the inward-dependency slice itself, and the
//! only place the three meet.
//!
//! **Mock-backed**: there is no `muxrd` gRPC round trip yet, and no
//! secure-storage plugin behind the preferences (see `crate::connection`'s
//! module docs and `docs/ARCHITECTURE.md`'s Planned section), so both `data`
//! repositories are in-memory. Everything above them — the async repository
//! contracts, the use cases, the controller's `ControllerCore::run` calls, the
//! screen's spawn/await/refresh path — is already the code the real sources
//! will run.
//!
//! The `connection` group is the one part with no repository behind it: those
//! facts come from [`crate::connection`]'s app-scoped controller, because more
//! than one feature reads them (see [`domain`]'s own note).

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
