//! The `pro` feature slice — muxr's single in-app purchase and the free-tier
//! rule it lifts.
//!
//! `{domain, data, presentation}`, dependencies pointing inward only
//! (`presentation` → `domain` ← `data`); [`di`] is the composition-root wiring
//! seam `crate::MuxrApp::init` calls into, not part of that slice.
//!
//! Unlike the other four slices this one owns no route: it publishes one
//! app-scoped [`presentation::controller::ProController`] every screen asks
//! "is this install Pro?" and "may it keep this server?", and the store this
//! app sells through sits behind [`domain::repositories::EntitlementRepository`]
//! so the whole rule is exercisable headless.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
