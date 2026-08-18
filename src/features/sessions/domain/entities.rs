//! `sessions` domain entities — the feature's plain-data types: one
//! aggregate ([`Session`]) plus the value objects the `/sessions` screen
//! renders from it. Real repository and mock alike map a session proto onto
//! these field by field, with no presentation-layer change.

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
    /// The tab/pane/client counts and pane previews the card's lower half
    /// renders.
    pub layout: LayoutSummary,
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

/// A session's layout, summarized — not the layout itself: the real
/// tab/pane tree belongs to the terminal feature, and this screen never
/// needs it.
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
}

impl LayoutSummary {
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
            layout: LayoutSummary {
                tabs: 4,
                panes: 9,
                third: ThirdMetric::Clients(0),
                preview: vec!["btop".to_string()],
            },
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
}
