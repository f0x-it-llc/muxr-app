use std::time::Duration;

use crate::ui::recency;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Stable daemon key; every mutation addresses a session by this, not the name.
    pub id: String,
    pub name: String,
    pub status: SessionStatus,
    pub backend: SessionBackend,
    pub age_secs: u64,
    pub has_bell: bool,
    pub is_current: bool,
    pub layout: LayoutSummary,
    /// `None` when the server reports no agents (no capability, zellij).
    pub agents: Option<crate::agents::AgentSummary>,
}

impl Session {
    /// `"?"` for an empty name.
    pub fn initial(&self) -> String {
        self.name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string())
    }

    pub fn age_label(&self) -> String {
        recency::age_label(Duration::from_secs(self.age_secs))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Resurrectible,
}

impl SessionStatus {
    pub fn label(self) -> &'static str {
        match self {
            SessionStatus::Running => "running",
            SessionStatus::Resurrectible => "resurrectible",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, SessionStatus::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionBackend {
    Zellij,
    Herdr,
}

impl SessionBackend {
    pub fn label(self) -> &'static str {
        match self {
            SessionBackend::Zellij => "zellij",
            SessionBackend::Herdr => "herdr",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSummary {
    pub tabs: u32,
    pub panes: u32,
    pub third: ThirdMetric,
    pub preview: Vec<String>,
    /// Wire order; empty for a resurrectible session or an unreadable layout.
    pub tabs_detail: Vec<TabSummary>,
    /// `None` today: `Layout` carries no space name on the wire.
    pub space: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSummary {
    pub index: u32,
    pub name: String,
    pub focused: bool,
    pub has_bell: bool,
    pub panes: Vec<PaneSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSummary {
    pub id: u32,
    pub title: String,
    pub cwd: Option<String>,
    pub focused: bool,
    pub floating: bool,
    pub exited: bool,
}

impl LayoutSummary {
    pub fn focused_tab(&self) -> Option<&TabSummary> {
        self.tabs_detail.iter().find(|tab| tab.focused)
    }

    pub fn panes_label(&self) -> String {
        format!("\u{25a3} {} panes", self.panes)
    }

    pub fn tabs_label(&self) -> String {
        format!("\u{25e7} {} tabs", self.tabs)
    }
}

/// Which fact the third span carries depends on session kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThirdMetric {
    Clients(u32),
    Spaces(u32),
    /// An idle session's stored count; glyphless because it is not a live metric.
    TabsSaved(u32),
}

impl ThirdMetric {
    pub fn label(self) -> String {
        match self {
            ThirdMetric::Clients(n) => format!("\u{25ce} {n} clients"),
            ThirdMetric::Spaces(n) => format!("\u{25c6} {n} spaces"),
            ThirdMetric::TabsSaved(n) => format!("{n} tabs saved"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateSessionParams {
    pub name: String,
    pub backend: SessionBackend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameRequest {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAgent {
    pub pane_id: u32,
    /// 0-based `TabMsg.position` in its space's layout; `None` when the layout cannot place the tab.
    pub tab_position: Option<u32>,
    pub space_id: Option<String>,
    pub space_name: Option<String>,
    pub tab_name: String,
    pub kind: String,
    pub name: Option<String>,
    pub display_kind: Option<String>,
    pub status: crate::agents::AgentStatus,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub change_seq: u64,
}

impl SessionAgent {
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.display_kind.as_deref())
            .unwrap_or(&self.kind)
    }

    pub fn subtitle(&self) -> Option<&str> {
        self.summary.as_deref().or(self.title.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> Session {
        Session {
            id: "s-1".to_string(),
            name: name.to_string(),
            status: SessionStatus::Running,
            backend: SessionBackend::Zellij,
            age_secs: 0,
            has_bell: false,
            is_current: false,
            layout: LayoutSummary {
                tabs: 4,
                panes: 9,
                third: ThirdMetric::Clients(0),
                preview: vec!["btop".to_string()],
                tabs_detail: Vec::new(),
                space: None,
            },
            agents: None,
        }
    }

    #[test]
    fn the_avatar_initial_is_the_uppercased_first_character() {
        assert_eq!(session("dev").initial(), "D");
        assert_eq!(session("Herdr").initial(), "H");
    }

    #[test]
    fn a_nameless_session_still_has_an_initial() {
        assert_eq!(session("").initial(), "?");
    }

    #[test]
    fn each_third_metric_prints_its_own_span() {
        assert_eq!(ThirdMetric::Clients(0).label(), "\u{25ce} 0 clients");
        assert_eq!(ThirdMetric::Spaces(5).label(), "\u{25c6} 5 spaces");
        assert_eq!(ThirdMetric::TabsSaved(0).label(), "0 tabs saved");
    }

    #[test]
    fn the_meta_spans_are_addressable_one_by_one() {
        let layout = session("dev").layout;
        assert_eq!(layout.panes_label(), "\u{25a3} 9 panes");
        assert_eq!(layout.tabs_label(), "\u{25e7} 4 tabs");
    }

    #[test]
    fn only_running_is_live() {
        assert!(SessionStatus::Running.is_live());
        assert!(!SessionStatus::Resurrectible.is_live());
    }

    fn aged(age_secs: u64) -> String {
        let mut s = session("dev");
        s.age_secs = age_secs;
        s.age_label()
    }

    #[test]
    fn age_wording_follows_the_recency_bands() {
        assert_eq!(aged(59), "just now");
        assert_eq!(aged(60), "1 minute ago");
        assert_eq!(aged(5 * 60), "5 minutes ago");
        assert_eq!(aged(3600), "1 hour ago");
        assert_eq!(aged(5 * 3600), "5 hours ago");
        assert_eq!(aged(24 * 3600), "yesterday");
        assert_eq!(aged(3 * 24 * 3600), "3 days ago");
        assert_eq!(aged(7 * 24 * 3600), "last week");
        assert_eq!(aged(21 * 24 * 3600), "3 weeks ago");
    }

    #[test]
    fn the_focused_tab_is_the_one_flagged_active() {
        let tab = |index, focused| TabSummary {
            index,
            name: format!("t{index}"),
            focused,
            has_bell: false,
            panes: Vec::new(),
        };
        let mut layout = session("dev").layout;
        assert!(layout.focused_tab().is_none());
        layout.tabs_detail = vec![tab(0, false), tab(1, true)];
        assert_eq!(layout.focused_tab().map(|t| t.index), Some(1));
    }
}
