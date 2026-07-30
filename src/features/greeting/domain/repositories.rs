//! The `greeting` feature's domain repository contract (huddle
//! clean-architecture refactor's `--arch clean-signals` scaffold).
//!
//! [`GreetingRepository`] is the seam between [`LoadGreeting`](super::use_cases::LoadGreeting)
//! and the data layer: `data::repositories::InMemoryGreetingRepository`
//! implements it over `data::sources::GreetingSource`, owning this
//! feature's single failure-mapping site. Consumed as
//! `Arc<dyn GreetingRepository + Send + Sync>` (the team-demo DI
//! convention), so neither the controller nor its use case carries a
//! repository type parameter.

use crate::failure::AppFailure;

/// The same dual `cfg_attr` every async trait in this scaffold uses (per
/// `docs/CODE_STANDARDS.md`) — `wasm32`'s single-threaded event loop can't
/// require `Send` futures.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait GreetingRepository {
    /// Load the greeting. Never fails in this scaffold — the mocked round
    /// trip and the (currently branch-free) failure boundary live in the
    /// data-layer implementation.
    async fn load_greeting(&self) -> Result<String, AppFailure>;
}