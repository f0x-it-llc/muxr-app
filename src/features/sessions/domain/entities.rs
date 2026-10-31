//! `sessions` domain entities — the feature's plain-data types: one
//! aggregate ([`Session`]) plus the value objects the `/sessions` screen
//! renders from it. Real repository and mock alike map a session proto onto
//! these field by field, with no presentation-layer change.

use std::time::Duration;

use crate::ui::recency;

/// One remote multiplexer session, as the sessions list renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Stable daemon key, distinct from [`name`](Self::name) — every
    /// mutation on [`SessionsRepository`](super::repositories::SessionsRepository)
    /// addresses a session by this, not the name.
    pub id: String,
    /// User-facing name (`.sess-name`); what a rename edits.
    pub name: String,
    /// Live or merely resurrectible (`.sess-card.running` vs `.idle`).
    pub status: SessionStatus,
    /// Which multiplexer is behind it (the `.chip-neutral` "zellij"/"herdr").
    pub backend: SessionBackend,
    /// Seconds since the session started, as the daemon reports it.
    pub age_secs: u64,
    /// Whether any tab of the session has rung the bell.
    pub has_bell: bool,
    /// Whether this is the session the daemon considers current (the one
    /// this device is attached to).
    pub is_current: bool,
    /// The tab/pane/client counts and pane previews the card's lower half
    /// renders.
    pub layout: LayoutSummary,
    /// Agent roll-up for the session; `None` when the server reports nothing
    /// (no capability, zellij), `Some` otherwise.
    pub agents: Option<crate::agents::AgentSummary>,
}

impl Session {
    /// Uppercased first character of [`name`](Self::name) for the avatar
    /// tile; `"?"` for empty (defence in depth — the repository rejects
    /// blank names).
    pub fn initial(&self) -> String {
        self.name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string())
    }

    /// Coarse age wording — the app-wide bands of [`crate::ui::recency`].
    pub fn age_label(&self) -> String {
        recency::age_label(Duration::from_secs(self.age_secs))
    }
}

/// A session's liveness — exactly the two states the daemon distinguishes here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// Live: the multiplexer process is up (`.chip-run` "running").
    Running,
    /// Saved but not running — attachable by resurrecting it.
    Resurrectible,
}

impl SessionStatus {
    pub fn label(self) -> &'static str {
        match self {
            SessionStatus::Running => "running",
            SessionStatus::Resurrectible => "resurrectible",
        }
    }

    /// Whether this is the live state — call sites key off this instead of
    /// re-matching the variant.
    pub fn is_live(self) -> bool {
        matches!(self, SessionStatus::Running)
    }
}

/// Which multiplexer fronts a session — `muxrd` speaks to both.
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

/// A session's layout, summarized — not the layout itself: the live
/// tab/pane tree belongs to the terminal feature; this keeps only the
/// read-only names and flags the sessions screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSummary {
    /// Open tabs (`◧ N tabs`).
    pub tabs: u32,
    /// Open panes across every tab (`▣ N panes`).
    pub panes: u32,
    /// The row's third, backend-dependent span — see [`ThirdMetric`].
    pub third: ThirdMetric,
    /// Up to two pane titles previewed as `.pane-pill`s; empty for an idle
    /// session (no pills row drawn).
    pub preview: Vec<String>,
    /// Every tab with its terminal panes, in wire order; empty for a
    /// resurrectible session or one whose layout could not be read.
    pub tabs_detail: Vec<TabSummary>,
    /// The layout's space name, when the wire carries one (`Layout` has no
    /// such field today, so real repositories leave this `None`).
    pub space: Option<String>,
}

/// One tab of a [`LayoutSummary`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSummary {
    /// The tab's position in the session.
    pub index: u32,
    pub name: String,
    /// Whether this is the session's active tab.
    pub focused: bool,
    pub has_bell: bool,
    /// Terminal panes only; plugin panes are omitted.
    pub panes: Vec<PaneSummary>,
}

/// One terminal pane of a [`TabSummary`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSummary {
    /// The wire pane id, unique within a tab's terminal panes.
    pub id: u32,
    pub title: String,
    /// Working directory, `None` when the backend reports none.
    pub cwd: Option<String>,
    pub focused: bool,
    pub floating: bool,
    pub exited: bool,
}

impl LayoutSummary {
    /// The active tab, if the layout lists one.
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

/// The `.sess-meta` row's third span: which fact it carries varies by session
/// kind — attached clients (running zellij), spaces (herdr), or saved tabs
/// (idle/resurrectible).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThirdMetric {
    /// `◎ N clients` — attached to a running zellij session.
    Clients(u32),
    /// `◆ N spaces` — a running herdr session's spaces.
    Spaces(u32),
    /// `N tabs saved` — an idle session's stored count; deliberately
    /// glyphless (not a live metric).
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

/// [`SessionsRepository::create_session`](super::repositories::SessionsRepository::create_session)'s
/// request — a named struct since a use case's `Params` is one associated type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateSessionParams {
    /// The new session's name (already trimmed by the dialog).
    pub name: String,
    /// Which multiplexer should host it.
    pub backend: SessionBackend,
}

/// [`SessionsRepository::rename_session`](super::repositories::SessionsRepository::rename_session)'s
/// request — a named struct, not a tuple, since a use case's `Params` is
/// one associated type (see [`RenameSession`](super::use_cases::RenameSession)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameRequest {
    /// The session to rename, by [`Session::id`].
    pub id: String,
    /// Its new name (already trimmed by the dialog that collected it).
    pub name: String,
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
