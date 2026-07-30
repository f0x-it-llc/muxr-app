//! The `terminal` feature slice — muxr's `/terminal` route: the live
//! terminal surface for a connected session. `{domain, data, presentation}`,
//! per the clean-architecture convention `examples/huddle` establishes (see
//! its `src/README-phase-c.md`): dependencies point inward only,
//! `presentation` → `domain` ← `data`. Empty today — Phase 2 wires the real
//! terminal I/O logic.

pub mod data;
pub mod domain;
pub mod presentation;
