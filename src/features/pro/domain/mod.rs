//! `pro` domain: framework-free; never imports `frust::` or a `data`/`presentation` path.
//! `entities` reads `connect`'s `SavedServer` instead of minting a second saved-server type.

pub mod entities;
pub mod repositories;
pub mod use_cases;
