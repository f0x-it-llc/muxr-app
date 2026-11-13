//! The `pro` slice: muxr's single in-app purchase and the free-tier rule it lifts.
//! Owns no route; `di` is the composition-root seam, not part of the slice.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
