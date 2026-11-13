//! Framework-free: never imports `frust::` or a `data`/`presentation` path.
//! [`entities`] re-exports [`crate::connection::TrustMode`].

pub mod entities;
pub mod keyring;
pub mod repositories;
pub mod use_cases;
