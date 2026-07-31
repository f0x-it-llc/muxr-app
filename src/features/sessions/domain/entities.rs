//! `sessions` domain entities — the feature's plain-data types
//! (framework-free, per `crate::features::sessions::domain`'s module doc).
//!
//! One aggregate ([`Session`]) plus the value objects the `/sessions` screen
//! renders from it. Every field here exists because `docs/redesign.html`'s
//! **sessions** block paints it: the card's name and avatar initial
//! ([`Session::name`]/[`Session::initial`]), its two chips
//! ([`Session::status`]/[`Session::backend`]), the `.sess-meta` counts row and
//! the `.pane-pill` previews ([`LayoutSummary`]), and the creation label
//! ([`Session::created`]). Nothing is modelled speculatively — a field with no
//! pixel behind it isn't here.
//!
//! # Mock era
//!
//! There is no `muxrd` gRPC round trip yet (see `crate::connection`'s module
//! docs), so these are the shapes the **mock** repository
//! (`crate::features::sessions::data`) produces. They are deliberately plain
//! owned data, so the real client can map a session proto onto them field by
//! field without the presentation layer changing at all.

/// One remote multiplexer session, as the sessions list renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The daemon's stable key for this session. Distinct from
    /// [`name`](Self::name) because a rename changes the name, not the key —
    /// every mutation on
    /// [`SessionsRepository`](super::repositories::SessionsRepository)
    /// addresses a session by this.
    pub id: String,
    /// The session's user-facing name (`.sess-name`), the one a rename edits.
    pub name: String,
    /// A pre-formatted creation label (`"2026-07-02"`) rather than a timestamp
    /// type: this crate has no date dependency, the daemon will send an
    /// already-human string, and the card only ever prints it.
    pub created: String,
    /// Whether the session is live or merely resurrectible (`.sess-card.running`
    /// vs `.sess-card.idle`).
    pub status: SessionStatus,
    /// Which multiplexer is behind it (the `.chip-neutral` "zellij"/"herdr").
    pub backend: SessionBackend,
    /// The tab/pane/client counts and pane previews the card's lower half
    /// renders.
    pub layout: LayoutSummary,
}

impl Session {
    /// The uppercase first character of [`name`](Self::name), for the card's
    /// `.sess-avatar` tile — `"?"` for an empty name (the repository rejects
    /// blank names, so this is defence in depth, not a reachable state).
    pub fn initial(&self) -> String {
        self.name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string())
    }
}

/// A session's liveness. Two states, because the daemon distinguishes exactly
/// two on this screen: attached-and-running, or saved-and-resurrectible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// Live: the multiplexer process is up (`.chip-run` "running").
    Running,
    /// Saved but not running — attachable by resurrecting it.
    Resurrectible,
}

impl SessionStatus {
    /// The chip label this status renders as.
    pub fn label(self) -> &'static str {
        match self {
            SessionStatus::Running => "running",
            SessionStatus::Resurrectible => "resurrectible",
        }
    }

    /// Whether this status is the live one — the card's success-hued chip keys
    /// off this rather than re-matching the variant at each paint site.
    pub fn is_live(self) -> bool {
        matches!(self, SessionStatus::Running)
    }
}

/// Which multiplexer fronts a session. `muxrd` speaks to both (see the app's
/// own README), and the card names which one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionBackend {
    /// A zellij session.
    Zellij,
    /// A herdr session.
    Herdr,
}

impl SessionBackend {
    /// The chip label this backend renders as.
    pub fn label(self) -> &'static str {
        match self {
            SessionBackend::Zellij => "zellij",
            SessionBackend::Herdr => "herdr",
        }
    }
}

/// A session's layout, summarized: the counts the `.sess-meta` row prints plus
/// the `.pane-pill` previews under it. A *summary*, not the layout itself —
/// the real tab/pane tree belongs to the terminal feature, and this screen
/// never needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSummary {
    /// Open tabs (`◧ N tabs`).
    pub tabs: u32,
    /// Open panes across every tab (`▣ N panes`).
    pub panes: u32,
    /// Clients currently attached (`◎ N clients`).
    pub clients: u32,
    /// Up to two pane titles the card previews as `.pane-pill`s. Empty for an
    /// idle session (the mock's `jumping-peach` row shows none).
    pub preview: Vec<String>,
}

impl LayoutSummary {
    /// The one-line counts summary the card's `.sess-meta` row prints.
    pub fn counts_line(&self) -> String {
        format!(
            "\u{25a3} {} panes   \u{25e7} {} tabs   \u{25ce} {} clients",
            self.panes, self.tabs, self.clients
        )
    }
}

/// [`SessionsRepository::rename_session`](super::repositories::SessionsRepository::rename_session)'s
/// two-field request — a named struct rather than a tuple because a use case's
/// `Params` is a single associated type (see
/// [`RenameSession`](super::use_cases::RenameSession)).
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
            created: "2026-07-02".to_string(),
            status: SessionStatus::Running,
            backend: SessionBackend::Zellij,
            layout: LayoutSummary {
                tabs: 4,
                panes: 9,
                clients: 0,
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
    fn the_counts_line_prints_every_count_the_redesign_shows() {
        let line = session("dev").layout.counts_line();
        assert!(line.contains("9 panes"), "{line}");
        assert!(line.contains("4 tabs"), "{line}");
        assert!(line.contains("0 clients"), "{line}");
    }

    #[test]
    fn only_running_is_live() {
        assert!(SessionStatus::Running.is_live());
        assert!(!SessionStatus::Resurrectible.is_live());
    }
}
