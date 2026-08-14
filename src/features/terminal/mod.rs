//! The `terminal` feature slice — muxr's `/terminal` route: the terminal
//! surface for the attached session, a pixel copy of `docs/terminal.html` v2.
//! `{domain, data, presentation}` with dependencies pointing inward only;
//! [`di`] is the composition-root wiring seam, outside that slice.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
