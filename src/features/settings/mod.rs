//! The `/settings` route: the connected server's settings screen.
//!
//! Dependencies point inward: `presentation` -> `domain` <- `data`.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
