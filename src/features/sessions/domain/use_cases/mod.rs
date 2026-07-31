//! `sessions` use cases — one file per `<verb><noun>` action (see
//! `examples/huddle`'s `domain/use_cases/` convention).
//!
//! Four, one per [`SessionsRepository`](super::repositories::SessionsRepository)
//! method: [`ListSessions`], [`CreateSession`], [`RenameSession`],
//! [`KillSession`]. Each is a thin, injected-repository
//! [`UseCase`](clean_signals::UseCase) with no logic of its own beyond
//! delegation — deliberately: the mock era has no business rule to encode yet,
//! and the value of the seam is that
//! [`SessionsController`](crate::features::sessions::presentation::controllers::SessionsController)
//! drives every call through `ControllerCore::run` (activity tracking, retry
//! policy, one failure-emission site) instead of touching a repository
//! directly. A real rule (a name-collision check, an optimistic patch) lands
//! in the matching file here without any call site moving.
//!
//! Never call [`UseCase::execute`](clean_signals::UseCase::execute) directly
//! from a controller — always drive it through
//! `ControllerCore::run`/`run_into` (huddle's rule, restated).

pub mod create_session;
pub mod kill_session;
pub mod list_sessions;
pub mod rename_session;

pub use create_session::CreateSession;
pub use kill_session::KillSession;
pub use list_sessions::ListSessions;
pub use rename_session::RenameSession;
