//! `connect` repository implementations — one file per concrete
//! `domain::repositories` trait implementation, each owning its own
//! failure-mapping site into [`crate::failure::AppFailure`]. No `frust::`
//! signals/widgets here — same framework-free rule as `domain` (see
//! `crate::features::connect::domain`'s module doc).
//!
//! | Module | Implements |
//! |---|---|
//! | [`mock_saved_servers`] | `SavedServersRepository`, in memory (mock-era — no persistence yet) |

pub mod mock_saved_servers;
