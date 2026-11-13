//! The app-level agent vocabulary: per-pane agent status and its session/tab roll-up.
//! Framework-free so sessions and terminal share it without importing each other.

use crate::net::proto;

/// Display order follows [`Self::priority`], not declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    /// Also what an unspecified wire value reads as.
    Unknown,
}

impl AgentStatus {
    /// herdr's display order.
    pub fn priority(self) -> u8 {
        match self {
            AgentStatus::Blocked => 4,
            AgentStatus::Done => 3,
            AgentStatus::Working => 2,
            AgentStatus::Idle => 1,
            AgentStatus::Unknown => 0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Blocked => "needs you",
            AgentStatus::Working => "working",
            AgentStatus::Done => "done",
            AgentStatus::Idle => "idle",
            AgentStatus::Unknown => "unknown",
        }
    }

    pub fn from_wire(value: i32) -> AgentStatus {
        match proto::AgentStatus::try_from(value) {
            Ok(proto::AgentStatus::Idle) => AgentStatus::Idle,
            Ok(proto::AgentStatus::Working) => AgentStatus::Working,
            Ok(proto::AgentStatus::Blocked) => AgentStatus::Blocked,
            Ok(proto::AgentStatus::Done) => AgentStatus::Done,
            Ok(proto::AgentStatus::Unknown | proto::AgentStatus::Unspecified) | Err(_) => {
                AgentStatus::Unknown
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSummary {
    pub total: u32,
    pub blocked: u32,
    pub done: u32,
    pub working: u32,
    pub idle: u32,
    pub unknown: u32,
    /// CLI labels, most urgent first.
    pub kinds: Vec<String>,
}

impl AgentSummary {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    pub fn has_blocked(&self) -> bool {
        self.blocked > 0
    }

    pub fn from_wire(wire: &proto::AgentSummary) -> AgentSummary {
        AgentSummary {
            total: wire.total,
            blocked: wire.blocked,
            done: wire.done,
            working: wire.working,
            idle: wire.idle,
            unknown: wire.unknown,
            kinds: wire.kinds.clone(),
        }
    }

    /// Kinds are distinct, ordered by their highest-priority agent.
    pub fn from_statuses<'a>(agents: impl Iterator<Item = (AgentStatus, &'a str)>) -> AgentSummary {
        let mut summary = AgentSummary::default();
        let mut kinds: Vec<(u8, &str)> = Vec::new();
        for (status, kind) in agents {
            summary.total += 1;
            match status {
                AgentStatus::Blocked => summary.blocked += 1,
                AgentStatus::Done => summary.done += 1,
                AgentStatus::Working => summary.working += 1,
                AgentStatus::Idle => summary.idle += 1,
                AgentStatus::Unknown => summary.unknown += 1,
            }
            match kinds.iter_mut().find(|(_, known)| *known == kind) {
                Some(entry) => entry.0 = entry.0.max(status.priority()),
                None => kinds.push((status.priority(), kind)),
            }
        }
        // Stable: equal priorities keep first-seen order.
        kinds.sort_by_key(|&(priority, _)| std::cmp::Reverse(priority));
        summary.kinds = kinds
            .into_iter()
            .map(|(_, kind)| kind.to_string())
            .collect();
        summary
    }
}

/// The desktop preview's agent inventory, shared by the terminal and sessions mocks.
#[doc(hidden)]
pub const PREVIEW_AGENT_MARKS: [(AgentStatus, &str); 4] = [
    (AgentStatus::Blocked, "claude"),
    (AgentStatus::Done, "codex"),
    (AgentStatus::Working, "gemini"),
    (AgentStatus::Idle, "claude"),
];

/// Most urgent status first, then the highest `change_seq`.
pub fn agent_sort_key(status: AgentStatus, change_seq: u64) -> (u8, u64) {
    (u8::MAX - status.priority(), u64::MAX - change_seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_orders_blocked_over_done_over_working_over_idle_over_unknown() {
        let order = [
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Working,
            AgentStatus::Idle,
            AgentStatus::Unknown,
        ];
        let priorities: Vec<u8> = order.iter().map(|s| s.priority()).collect();
        assert_eq!(priorities, [4, 3, 2, 1, 0]);
    }

    #[test]
    fn labels_are_the_design_words() {
        let labels: Vec<&str> = [
            AgentStatus::Blocked,
            AgentStatus::Working,
            AgentStatus::Done,
            AgentStatus::Idle,
            AgentStatus::Unknown,
        ]
        .iter()
        .map(|s| s.label())
        .collect();
        assert_eq!(labels, ["needs you", "working", "done", "idle", "unknown"]);
    }

    #[test]
    fn from_wire_maps_every_value_and_reads_unspecified_as_unknown() {
        let wire = |s: proto::AgentStatus| AgentStatus::from_wire(s as i32);
        assert_eq!(wire(proto::AgentStatus::Idle), AgentStatus::Idle);
        assert_eq!(wire(proto::AgentStatus::Working), AgentStatus::Working);
        assert_eq!(wire(proto::AgentStatus::Blocked), AgentStatus::Blocked);
        assert_eq!(wire(proto::AgentStatus::Done), AgentStatus::Done);
        assert_eq!(wire(proto::AgentStatus::Unknown), AgentStatus::Unknown);
        assert_eq!(wire(proto::AgentStatus::Unspecified), AgentStatus::Unknown);
        assert_eq!(AgentStatus::from_wire(999), AgentStatus::Unknown);
    }

    #[test]
    fn from_statuses_counts_each_status_and_collects_distinct_kinds_by_urgency() {
        let summary = AgentSummary::from_statuses(
            [
                (AgentStatus::Idle, "gemini"),
                (AgentStatus::Working, "claude"),
                (AgentStatus::Blocked, "codex"),
                (AgentStatus::Done, "claude"),
            ]
            .into_iter(),
        );
        assert_eq!(summary.total, 4);
        assert_eq!(
            (summary.blocked, summary.done, summary.working, summary.idle),
            (1, 1, 1, 1)
        );
        assert_eq!(summary.unknown, 0);
        assert_eq!(summary.kinds, ["codex", "claude", "gemini"]);
        assert!(summary.has_blocked());
        assert!(!summary.is_empty());
    }

    #[test]
    fn an_empty_roll_up_is_empty_and_blocks_nothing() {
        let summary = AgentSummary::from_statuses(std::iter::empty());
        assert!(summary.is_empty());
        assert!(!summary.has_blocked());
        assert!(summary.kinds.is_empty());
    }

    #[test]
    fn from_wire_copies_counts_and_kinds() {
        let wire = proto::AgentSummary {
            total: 3,
            blocked: 1,
            done: 0,
            working: 1,
            idle: 0,
            unknown: 1,
            kinds: vec!["claude".into(), "pi".into()],
        };
        let summary = AgentSummary::from_wire(&wire);
        assert_eq!((summary.total, summary.blocked, summary.working), (3, 1, 1));
        assert_eq!(summary.unknown, 1);
        assert_eq!(summary.kinds, ["claude", "pi"]);
    }

    #[test]
    fn sort_key_puts_urgent_first_then_most_recent_change() {
        let mut rows = [
            (AgentStatus::Idle, 9),
            (AgentStatus::Blocked, 1),
            (AgentStatus::Working, 5),
            (AgentStatus::Working, 7),
            (AgentStatus::Done, 2),
        ];
        rows.sort_by_key(|&(status, seq)| agent_sort_key(status, seq));
        assert_eq!(
            rows,
            [
                (AgentStatus::Blocked, 1),
                (AgentStatus::Done, 2),
                (AgentStatus::Working, 7),
                (AgentStatus::Working, 5),
                (AgentStatus::Idle, 9),
            ]
        );
    }
}
