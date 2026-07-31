//! The `terminal` feature slice — muxr's `/terminal` route: the live
//! terminal surface for a connected session. `{domain, data, presentation}`,
//! per the clean-architecture convention `examples/huddle` establishes (see
//! its `src/README-phase-c.md`): dependencies point inward only,
//! `presentation` → `domain` ← `data`. Empty today — Phase 2 wires the real
//! terminal I/O logic. [`di`] is the composition-root wiring seam
//! `crate::MuxrApp::init` calls into (see its module docs) — not part of the
//! inward-dependency slice itself.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
