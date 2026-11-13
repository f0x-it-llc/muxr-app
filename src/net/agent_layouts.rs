//! The `ListAgents` + per-space `GetLayout` join both features' repositories share. A space
//! whose read fails is reported, so its rows stay unresolved rather than dropped. [`tab_label`]
//! and [`unique_tab_key`] are the one definition of a tab key, shared with the terminal's tabs.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use tonic::Status;

use crate::net::MuxrClient;
use crate::net::proto::{AgentMsg, Layout, SessionRef};

/// Followed by the tab's 1-based position (`tab 3`).
const UNNAMED_TAB: &str = "tab";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSlot {
    pub tab_key: String,
    /// 0-based.
    pub position: u32,
    pub name: String,
}

/// The empty space id is a backend without spaces.
pub type TabSlots = HashMap<(String, u64), TabSlot>;

#[derive(Debug)]
pub struct SpaceFailure {
    pub space_id: String,
    pub status: Status,
}

#[derive(Debug, Default)]
pub struct AgentLayouts {
    pub tabs: TabSlots,
    pub failed: Vec<SpaceFailure>,
}

impl AgentLayouts {
    pub fn space_failed(&self, space_id: &str) -> bool {
        self.failed
            .iter()
            .any(|failure| failure.space_id == space_id)
    }
}

pub fn agent_spaces(agents: &[AgentMsg]) -> Vec<String> {
    let mut spaces: Vec<String> = Vec::new();
    for agent in agents {
        if !spaces.contains(&agent.space_id) {
            spaces.push(agent.space_id.clone());
        }
    }
    spaces
}

/// `request`'s own `space_id` is overwritten per call.
pub async fn fetch_agent_layouts(
    client: &MuxrClient,
    request: &SessionRef,
    spaces: Vec<String>,
) -> AgentLayouts {
    join_layouts(spaces, |space_id| {
        let mut layout_request = request.clone();
        layout_request.space_id = space_id;
        let client = client.clone();
        async move {
            let mut stub = client.authed();
            stub.get_layout(layout_request)
                .await
                .map(tonic::Response::into_inner)
        }
    })
    .await
}

pub async fn join_layouts<F, Fut>(spaces: Vec<String>, fetch: F) -> AgentLayouts
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Layout, Status>>,
{
    let reads = spaces.iter().map(|space| fetch(space.clone())).collect();
    let mut answer = AgentLayouts::default();
    for (space_id, read) in spaces.into_iter().zip(join_all(reads).await) {
        match read {
            Ok(layout) => answer.tabs.extend(
                layout_slots(&layout)
                    .into_iter()
                    .map(|(tab_id, slot)| ((space_id.clone(), tab_id), slot)),
            ),
            Err(status) => answer.failed.push(SpaceFailure { space_id, status }),
        }
    }
    answer
}

pub fn layout_slots(layout: &Layout) -> Vec<(u64, TabSlot)> {
    let mut taken: Vec<String> = Vec::with_capacity(layout.tabs.len());
    layout
        .tabs
        .iter()
        .map(|tab| {
            let index = tab.position.saturating_add(1);
            let slot = TabSlot {
                tab_key: unique_tab_key(&tab.name, index, &mut taken),
                position: tab.position,
                name: tab.name.clone(),
            };
            (u64::from(tab.tab_id), slot)
        })
        .collect()
}

/// `index` is the 1-based position.
pub fn tab_label(name: &str, index: u32) -> String {
    let name = name.trim();
    if name.is_empty() {
        format!("{UNNAMED_TAB} {index}")
    } else {
        name.to_string()
    }
}

/// A repeated name gets its 1-based position: the first `zsh` stays `zsh`, a later one `zsh 4`.
pub fn unique_tab_key(name: &str, index: u32, taken: &mut Vec<String>) -> String {
    let base = tab_label(name, index);
    let key = if taken.contains(&base) {
        format!("{base} {index}")
    } else {
        base
    };
    taken.push(key.clone());
    key
}

/// Production `tokio` carries no `join_all`.
async fn join_all<Fut: Future>(futures: Vec<Fut>) -> Vec<Fut::Output> {
    let mut pending: Vec<Option<Pin<Box<Fut>>>> =
        futures.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut outputs: Vec<Option<Fut::Output>> = pending.iter().map(|_| None).collect();
    std::future::poll_fn(move |cx| {
        let mut done = true;
        for (slot, output) in pending.iter_mut().zip(outputs.iter_mut()) {
            if let Some(future) = slot {
                match future.as_mut().poll(cx) {
                    Poll::Ready(value) => {
                        *output = Some(value);
                        *slot = None;
                    }
                    Poll::Pending => done = false,
                }
            }
        }
        if done {
            Poll::Ready(outputs.iter_mut().filter_map(Option::take).collect())
        } else {
            Poll::Pending
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::net::proto::TabMsg;

    fn tab(position: u32, tab_id: u32, name: &str) -> TabMsg {
        TabMsg {
            position,
            tab_id,
            name: name.to_string(),
            ..TabMsg::default()
        }
    }

    fn block_on<T>(fut: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn a_layouts_slots_carry_unique_keys_positions_and_names() {
        let layout = Layout {
            tabs: vec![tab(0, 7, "zsh"), tab(1, 8, "zsh"), tab(2, 9, " ")],
        };
        let slots = layout_slots(&layout);
        let keys: Vec<_> = slots
            .iter()
            .map(|(id, s)| (*id, s.tab_key.as_str()))
            .collect();
        assert_eq!(keys, [(7, "zsh"), (8, "zsh 2"), (9, "tab 3")]);
        assert_eq!((slots[1].1.position, slots[1].1.name.as_str()), (1, "zsh"));
    }

    #[test]
    fn the_join_reads_every_space_concurrently_and_names_the_failed_ones() {
        let started = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&started);
        let answer = block_on(join_layouts(
            vec!["a".to_string(), "b".to_string(), String::new()],
            move |space| {
                let seen = Arc::clone(&seen);
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    // Every read is in flight before any lands.
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    assert_eq!(seen.load(Ordering::SeqCst), 3);
                    match space.as_str() {
                        "b" => Err(Status::unavailable("down")),
                        _ => Ok(Layout {
                            tabs: vec![tab(0, 1, &format!("t{space}"))],
                        }),
                    }
                }
            },
        ));
        assert_eq!(answer.tabs[&("a".to_string(), 1)].tab_key, "ta");
        assert_eq!(answer.tabs[&(String::new(), 1)].tab_key, "t");
        assert!(answer.space_failed("b") && !answer.space_failed("a"));
        assert_eq!(answer.failed.len(), 1);
    }

    #[test]
    fn agent_spaces_are_distinct_in_first_seen_order() {
        let agent = |space: &str| AgentMsg {
            space_id: space.to_string(),
            ..AgentMsg::default()
        };
        let spaces = agent_spaces(&[agent("b"), agent("a"), agent("b"), agent("")]);
        assert_eq!(spaces, ["b", "a", ""]);
    }
}
