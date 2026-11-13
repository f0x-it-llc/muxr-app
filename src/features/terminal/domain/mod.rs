//! Framework-free: imports only this feature's `domain`, `crate::failure`, and two re-exported
//! sibling `domain` items (`sessions`' name contract, `settings`' cursor preferences), so
//! `presentation` names only `terminal::domain`.

pub mod entities;
pub mod feed;
pub mod repositories;
pub mod use_cases;
