//! The `sessions` feature slice — muxr's `/sessions` route: the list of
//! active/recent terminal sessions. `{domain, data, presentation}`, per the
//! clean-architecture convention `examples/huddle` establishes (see its
//! `src/README-phase-c.md`): dependencies point inward only, `presentation`
//! → `domain` ← `data`. Empty today — Phase 2 wires the real session-list
//! logic.

pub mod data;
pub mod domain;
pub mod presentation;
