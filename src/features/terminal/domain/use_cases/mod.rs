//! `terminal` use cases — one file per `<verb><noun>` action (see
//! `examples/huddle`'s `domain/use_cases/` convention). Five, covering everything
//! `docs/terminal.html` v2 can do: [`LoadWorkspace`] (arrival, and every reload a
//! space switch drives), [`SwitchTab`] (the tab step of the sheet's pane commit),
//! [`ListSpaceTabs`] (a read-only peek at a non-selected space's tabs),
//! [`RenameAttachedSession`] and [`DetachSession`] (the app bar's overflow menu).
//! Each is a thin, injected-repository [`UseCase`](clean_signals::UseCase) with no
//! logic of its own beyond delegation: the value of the seam is that
//! [`TerminalController`](crate::features::terminal::presentation::controller::TerminalController)
//! drives every call through `ControllerCore::run` (activity tracking, retry policy,
//! one failure-emission site) instead of touching a repository directly.
//!
//! Two of them reach the **`sessions`** feature's repository rather than this one's
//! — see [`LoadWorkspace`]/[`RenameAttachedSession`] for why a session's name has
//! exactly one owner. Never call
//! [`UseCase::execute`](clean_signals::UseCase::execute) directly from a controller
//! — always drive it through `ControllerCore::run`/`run_into`.

pub mod detach_session;
pub mod list_space_tabs;
pub mod load_workspace;
pub mod rename_session;
pub mod switch_tab;

pub use detach_session::DetachSession;
pub use list_space_tabs::ListSpaceTabs;
pub use load_workspace::{LoadWorkspace, WorkspaceQuery};
pub use rename_session::RenameAttachedSession;
pub use switch_tab::SwitchTab;
