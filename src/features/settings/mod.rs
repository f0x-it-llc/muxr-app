//! The `settings` feature slice — muxr's `/settings` route: the connected
//! server's own settings screen. Five groups (`connection`, `auth tokens`,
//! `saved servers`, `terminal appearance`, `session poll`) plus Disconnect,
//! with the reusable picker sheet the last two groups' rows open.
//!
//! `{domain, data, presentation}`, dependencies pointing inward only,
//! `presentation` → `domain` ← `data`. [`di`] is the composition-root wiring
//! seam `crate::MuxrApp::init` calls into — not part of the inward-dependency
//! slice itself, and the only place the three meet.
//!
//! `GrpcAuthTokensRepository`/`PrefsRepository` back a production run for
//! real; the mock pair behind `di.rs`'s test-only seam is what every headless
//! suite runs on. The `connection` group is the one part with no repository
//! behind it at all: those facts come from [`crate::connection`]'s app-scoped
//! controller, because more than one feature reads them (see [`domain`]'s
//! own note).

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
