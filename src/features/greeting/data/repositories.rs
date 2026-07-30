//! [`InMemoryGreetingRepository`] — the [`GreetingRepository`]
//! implementation this scaffold wires up (huddle clean-architecture
//! refactor's `--arch clean-signals` scaffold).
//!
//! Adapts [`GreetingSource`] to the greeting domain trait, owning this
//! feature's single failure-mapping site ([`map_source_error`]) — the
//! **only** place a [`SourceError`] is converted into an
//! [`AppFailure`]. This is the **only** greeting file that mentions
//! [`crate::features::greeting::data::sources`] (the inward-dependency
//! rule: nothing outside `data/` mentions the source).

use crate::failure::AppFailure;
use crate::features::greeting::data::sources::{GreetingSource, SourceError};
use crate::features::greeting::domain::repositories::GreetingRepository;

/// The source-backed [`GreetingRepository`].
#[derive(Clone, Copy, Default)]
pub struct InMemoryGreetingRepository {
    source: GreetingSource,
}

impl InMemoryGreetingRepository {
    /// A fresh repository handle over a fresh [`GreetingSource`].
    pub fn new() -> Self {
        Self {
            source: GreetingSource::new(),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl GreetingRepository for InMemoryGreetingRepository {
    async fn load_greeting(&self) -> Result<String, AppFailure> {
        self.source.fetch_greeting().await.map_err(map_source_error)
    }
}

/// This feature's single transport→failure mapping site (the team-demo
/// `map_transport_error` convention): every [`SourceError`] variant maps to
/// exactly one [`AppFailure`] here, and nowhere else.
fn map_source_error(err: SourceError) -> AppFailure {
    match err {
        SourceError::Unavailable(msg) => AppFailure::Network(msg),
    }
}