//! The `settings` feature slice — muxr's `/settings` route: app preferences.
//! `{domain, data, presentation}`, per the clean-architecture convention
//! `examples/huddle` establishes (see its `src/README-phase-c.md`):
//! dependencies point inward only, `presentation` → `domain` ← `data`. Empty
//! today — Phase 2 wires the real settings logic.

pub mod data;
pub mod domain;
pub mod presentation;
