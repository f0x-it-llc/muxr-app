//! One thin file per
//! [`SessionsRepository`](super::repositories::SessionsRepository) method —
//! [`ListSessions`], [`CreateSession`], [`RenameSession`], [`KillSession`] —
//! each an injected-repository [`UseCase`](clean_signals::UseCase) with no
//! logic beyond delegation. Call only through `ControllerCore::run`/
//! `run_into`, never [`UseCase::execute`](clean_signals::UseCase::execute) directly.

pub mod create_session;
pub mod kill_session;
pub mod list_sessions;
pub mod rename_session;

pub use create_session::CreateSession;
pub use kill_session::KillSession;
pub use list_sessions::ListSessions;
pub use rename_session::RenameSession;
