//! `greeting` data layer (huddle clean-architecture refactor's
//! `--arch clean-signals` scaffold): [`sources::GreetingSource`] (the
//! in-memory transport stand-in, with its internal
//! [`sources::SourceError`]) and [`repositories::InMemoryGreetingRepository`]
//! (the [`GreetingRepository`](super::domain::GreetingRepository)
//! implementation, owning this feature's single failure-mapping site).

pub mod repositories;
pub mod sources;