//! The `terminal` feature slice — muxr's `/terminal` route: the terminal
//! surface for the session the user attached to, a pixel copy of
//! `docs/terminal.html` **v2** ("One less row"). `{domain, data,
//! presentation}`, per the clean-architecture convention `examples/huddle`
//! establishes (see its `src/README-phase-c.md`): dependencies point inward
//! only, `presentation` → `domain` ← `data`. [`di`] is the composition-root
//! wiring seam `crate::MuxrApp::init` calls into (see its module docs) — not
//! part of the inward-dependency slice itself.
//!
//! Mock-era, like the two screens before it: `data`'s repository is seeded
//! verbatim from that mock's own `TABS` map, and the tab world is deliberately
//! the same whichever session was attached (see `domain::entities`). What is
//! real is every interaction — the fullscreen transition, the tab switch and
//! its replayed reveal, the rename that writes through the **sessions**
//! feature's own repository, and the detach that pops back to the list.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
