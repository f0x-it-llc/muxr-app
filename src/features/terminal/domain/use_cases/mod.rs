//! Drive every use case through `ControllerCore::run`/`run_into`, never `UseCase::execute`
//! directly.

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
