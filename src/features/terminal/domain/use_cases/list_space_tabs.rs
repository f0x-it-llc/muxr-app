use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::TabEntry;
use crate::features::terminal::domain::repositories::TerminalRepository;

/// Reads another space's tabs without moving this connection's focus.
pub struct ListSpaceTabs {
    repo: Arc<dyn TerminalRepository + Send + Sync>,
}

impl ListSpaceTabs {
    pub fn new(repo: Arc<dyn TerminalRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for ListSpaceTabs {
    type Params = String;
    type Output = Vec<TabEntry>;
    type Failure = AppFailure;

    async fn execute(&self, space_id: String) -> Result<Vec<TabEntry>, AppFailure> {
        self.repo.list_tabs_in_space(&space_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::terminal::domain::entities::{PaneEntry, Space, TermLine};

    struct StubRepository {
        answer: fn(&str) -> Result<Vec<TabEntry>, AppFailure>,
    }

    #[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
    #[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
    impl TerminalRepository for StubRepository {
        async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
            unreachable!("ListSpaceTabs never lists spaces")
        }

        async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
            unreachable!("ListSpaceTabs never lists the selected space's tabs directly")
        }

        async fn list_tabs_in_space(&self, space_id: &str) -> Result<Vec<TabEntry>, AppFailure> {
            (self.answer)(space_id)
        }

        async fn tab_lines(&self, _key: String) -> Result<Vec<TermLine>, AppFailure> {
            unreachable!("ListSpaceTabs never reads lines")
        }

        async fn switch_space(&self, _space_id: String) -> Result<(), AppFailure> {
            unreachable!("ListSpaceTabs never switches — it is a read-only peek")
        }

        async fn go_to_tab(&self, _tab_id: u64) -> Result<(), AppFailure> {
            unreachable!("ListSpaceTabs never switches tabs")
        }

        async fn detach_session(&self, _id: String) -> Result<(), AppFailure> {
            unreachable!("ListSpaceTabs never detaches")
        }
    }

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    fn tab(key: &str) -> TabEntry {
        TabEntry {
            index: 1,
            tab_id: 1,
            key: key.to_string(),
            label: key.to_string(),
            pane_title: String::new(),
            pane_name: String::new(),
            pane_path: String::new(),
            panes: Vec::<PaneEntry>::new(),
            active: false,
            has_bell: false,
            floating_panes_visible: false,
            fullscreen_active: false,
        }
    }

    #[test]
    fn it_forwards_the_space_id_and_answers_the_repositorys_tabs() {
        let use_case = ListSpaceTabs::new(Arc::new(StubRepository {
            answer: |space_id| {
                assert_eq!(space_id, "zelli_mobile");
                Ok(vec![tab("logs"), tab("worker")])
            },
        }));

        let tabs = block_on(use_case.execute("zelli_mobile".to_string()))
            .expect("the stub answers Ok for zelli_mobile");
        let keys: Vec<&str> = tabs.iter().map(|tab| tab.key.as_str()).collect();
        assert_eq!(keys, ["logs", "worker"]);
    }

    #[test]
    fn a_repository_failure_surfaces_unmapped_as_the_shared_appfailure_vocabulary() {
        let use_case = ListSpaceTabs::new(Arc::new(StubRepository {
            answer: |space_id| {
                Err(AppFailure::Internal(format!(
                    "list_tabs_in_space: no space {space_id}"
                )))
            },
        }));

        assert!(matches!(
            block_on(use_case.execute("nope".to_string())),
            Err(AppFailure::Internal(_))
        ));
    }
}
