//! The `sessions` feature slice — muxr's `/sessions` route: the list of a
//! connected server's terminal sessions, with create/rename/kill (a card tap
//! attaches, a long-press opens the management sheet those two live in), the
//! fixed server-info + latency strip, and the attach path into `/terminal`.
//!
//! The save-credential offer used to be this screen's too (a banner over the
//! list). It is answered on Connect now, before the redirect that lands here —
//! see [`crate::connection`]'s "Credential offer placement".
//!
//! `{domain, data, presentation}`, per the clean-architecture convention
//! `examples/huddle` establishes (see its `src/README-phase-c.md`):
//! dependencies point inward only, `presentation` → `domain` ← `data`. [`di`]
//! is the composition-root wiring seam `crate::MuxrApp::init` calls into (see
//! its module docs) — not part of the inward-dependency slice itself, and the
//! only place the three meet.
//!
//! **`GrpcSessionsRepository`** (`data::repositories`) is the production
//! `data` implementation, over `muxrd`'s real gRPC round trip (see
//! `crate::connection`'s module docs); `di::provide` publishes it unless a
//! test process opted out via `di::use_mock_sessions_for_tests`, in which case
//! `MockSessionsRepository` stands in instead. The async repository contract,
//! the use cases, the controller's `ControllerCore::run` calls and the
//! screen's spawn/await/refresh path are identical either way — the mock
//! exists for headless suites, not as a placeholder for the real client.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
