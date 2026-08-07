//! The `connect` feature slice — muxr's `/` route: the connect flow (server
//! address + login token, TLS posture, saved servers, QR pairing).
//! `{domain, data, presentation}`, per the clean-architecture convention
//! `examples/huddle` establishes (see its `src/README-phase-c.md`): dependencies
//! point inward only, `presentation` → `domain` ← `data`. [`di`] is the
//! composition-root wiring seam `crate::MuxrApp::init` calls into (see its
//! module docs) — not part of the inward-dependency slice itself.
//!
//! | Layer | What it holds |
//! |---|---|
//! | [`domain`] | `SavedServer`, the `SavedServersRepository` contract, the `host:port` validation |
//! | [`data`] | `MockSavedServersRepository` — in-memory, seeded (no persistence yet) |
//! | [`presentation`] | The `ConnectScreen` `Component`, its `ConnectController`, and the eight pieces |
//! | [`di`] | Constructs the repository once and publishes it into context |
//!
//! **Partly mock-era still.** Credential persistence and biometrics are
//! absent (the saved-server store is in memory), so nothing a scan or a form
//! collects outlives the process. The QR sheet itself is real: it scans a
//! `muxr://pair` code through `frust-camera`, prefills the form and offers the
//! explicit pinned connect (see [`presentation::pages`]' QR-sheet section).
//! See `docs/ARCHITECTURE.md`'s Planned section.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
