//! `sessions` use cases — one thin file per
//! [`SessionsRepository`](super::repositories::SessionsRepository) method:
//! [`ListSessions`], [`CreateSession`], [`RenameSession`], [`KillSession`].
//! Each is an injected-repository [`UseCase`](clean_signals::UseCase) with
//! no logic beyond delegation — the seam's value is that
//! [`SessionsController`](crate::features::sessions::presentation::controllers::SessionsController)
//! drives every call through `ControllerCore::run` rather than touching a
//! repository directly; a real rule lands in the matching file with no call
//! site moving. Never call [`UseCase::execute`](clean_signals::UseCase::execute)
//! directly from a controller — always through `ControllerCore::run`/`run_into`.

pub mod create_session;
pub mod kill_session;
pub mod list_sessions;
pub mod rename_session;

pub use create_session::CreateSession;
pub use kill_session::KillSession;
pub use list_sessions::ListSessions;
pub use rename_session::RenameSession;
