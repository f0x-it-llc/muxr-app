//! `sessions` domain entities — the feature's plain-data types.
//!
//! One aggregate ([`Session`]) plus the value objects the `/sessions` screen
//! renders from it. Every field exists because `docs/redesign.html`'s
//! **sessions** block paints it: the card's name and avatar initial, its two
//! chips ([`Session::status`]/[`Session::backend`]), and the `.sess-meta`
//! counts row plus the `.pane-pill` previews ([`LayoutSummary`]). Nothing is
//! modelled speculatively — there is no creation timestamp, since the
//! redesigned card prints no `created` line.
//!
//! # The varying third metric
//!
//! The `.sess-meta` row's first two spans are always panes and tabs; the
//! third **varies by what the session actually is** — `◎ N clients` for a
//! running zellij session, `◆ N spaces` for a herdr one, `N tabs saved` for
//! an idle resurrectible one. That is a difference in the *facts a daemon
//! reports*, not a rendering choice, so it is modelled as [`ThirdMetric`] —
//! one enum on [`LayoutSummary`] rather than three mostly-zero count fields.
//!
//! These shapes are plain owned data — real repository or mock alike map a
//! session proto onto them field by field, with no presentation-layer change.

/// One remote multiplexer session, as the sessions list renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The daemon's stable key for this session, distinct from
    /// [`name`](Self::name) (a rename changes the name, not the key) — every
    /// mutation on [`SessionsRepository`](super::repositories::SessionsRepository)
    /// addresses a session by this.
    pub id: String,
    /// The session's user-facing name (`.sess-name`), the one a rename edits.
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
    /// The `.sess-meta` row's varying third span — see [`ThirdMetric`] and the
    /// [module docs](self).
    pub third: ThirdMetric,
    /// Up to two pane titles the card previews as `.pane-pill`s. Empty for an
    /// idle session (the mock's `jumping-peach` row shows none, and the card
    /// draws no pills row at all for one).
    pub preview: Vec<String>,
}

impl LayoutSummary {
    /// The `▣ N panes` span.
    pub fn panes_label(&self) -> String {
        format!("\u{25a3} {} panes", self.panes)
    }

    /// The `◧ N tabs` span.
    pub fn tabs_label(&self) -> String {
        format!("\u{25e7} {} tabs", self.tabs)
    }

    /// All three `.sess-meta` spans on one line, as against the card's own
    /// three-span row, which lays each label out separately with the
    /// redesign's 12px gap.
    pub fn counts_line(&self) -> String {
        format!(
            "{}   {}   {}",
            self.panes_label(),
            self.tabs_label(),
            self.third.label()
        )
    }
}

/// The `.sess-meta` row's third span: **which** fact it carries depends on the
/// session, not on the screen (see the [module docs](self)). A daemon reports
/// attached clients for a running zellij session, spaces for a herdr one, and
/// how many tabs a resurrectible session has saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThirdMetric {
    /// `◎ N clients` — clients currently attached to a running zellij session.
    Clients(u32),
    /// `◆ N spaces` — a running herdr session's spaces.
    Spaces(u32),
    /// `N tabs saved` — what an idle, resurrectible session kept. Glyphless in
    /// the redesign, deliberately: it is a stored count, not a live one.
    TabsSaved(u32),
}

impl ThirdMetric {
    /// The span this metric renders as.
    pub fn label(self) -> String {
        match self {
            ThirdMetric::Clients(n) => format!("\u{25ce} {n} clients"),
            ThirdMetric::Spaces(n) => format!("\u{25c6} {n} spaces"),
            ThirdMetric::TabsSaved(n) => format!("{n} tabs saved"),
        }
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
    fn the_counts_line_prints_every_count_the_redesign_shows() {
        let line = session("dev").layout.counts_line();
        assert!(line.contains("9 panes"), "{line}");
        assert!(line.contains("4 tabs"), "{line}");
        assert!(line.contains("0 clients"), "{line}");
    }

    #[test]
    fn each_third_metric_prints_its_own_span() {
        // The three shapes `docs/redesign.html`'s three cards draw, in order.
        assert_eq!(ThirdMetric::Clients(0).label(), "\u{25ce} 0 clients");
        assert_eq!(ThirdMetric::Spaces(5).label(), "\u{25c6} 5 spaces");
        assert_eq!(ThirdMetric::TabsSaved(0).label(), "0 tabs saved");
    }

    #[test]
    fn the_meta_spans_are_addressable_one_by_one() {
        // The card lays the three spans out separately (12px gap), so each
        // has to be printable on its own — `counts_line` is the joined form.
        let layout = session("dev").layout;
        assert_eq!(layout.panes_label(), "\u{25a3} 9 panes");
        assert_eq!(layout.tabs_label(), "\u{25e7} 4 tabs");
        let line = layout.counts_line();
        assert!(line.starts_with(&layout.panes_label()), "{line}");
        assert!(line.ends_with(&layout.third.label()), "{line}");
    }

    #[test]
    fn only_running_is_live() {
        assert!(SessionStatus::Running.is_live());
        assert!(!SessionStatus::Resurrectible.is_live());
    }
}
