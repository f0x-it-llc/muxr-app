//! Re-exports rather than re-mints `RenameRequest` and the settings cursor style: a session's
//! name and a cursor shape each have one owner.

pub use crate::agents::AgentStatus;
pub use crate::features::sessions::domain::entities::RenameRequest;
pub use crate::features::settings::domain::entities::CursorStyle as SettingsCursorStyle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Space {
    /// Opaque backend id, stable across renames; the in-memory mock uses names as ids.
    pub id: String,
    pub name: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneEntry {
    /// Unique only within its own [`is_plugin`](Self::is_plugin) namespace.
    pub id: u32,
    pub title: String,
    pub cwd: String,
    pub focused: bool,
    pub floating: bool,
    pub exited: bool,
    /// Picks the wire's `PaneId::Plugin` or `PaneId::Terminal` for pane-targeted verbs.
    pub is_plugin: bool,
    /// zellij flags EVERY client-active pane while the tab is fullscreen (union `is_focused`); the
    /// tab truth is [`TabEntry::fullscreen_active`], the fullscreened pane is the focused one.
    pub is_fullscreen: bool,
    /// Geometry in cells: left edge, top edge, height, width.
    pub x: u32,
    pub y: u32,
    pub rows: u32,
    pub cols: u32,
    pub agent: Option<AgentMark>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMark {
    pub kind: String,
    pub name: Option<String>,
    pub status: AgentStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEntry {
    pub pane_id: u32,
    pub tab_id: u64,
    /// Empty when the tab is not in its space's layout.
    pub tab_key: String,
    pub space_id: Option<String>,
    pub space_name: Option<String>,
    pub tab_name: String,
    pub kind: String,
    pub name: Option<String>,
    pub display_kind: Option<String>,
    pub status: AgentStatus,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub cwd: Option<String>,
    pub focused: bool,
    /// Advances whenever the agent's state changes; larger is more recent.
    pub change_seq: u64,
}

impl AgentEntry {
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.display_kind.as_deref())
            .unwrap_or(&self.kind)
    }

    pub fn subtitle(&self) -> Option<&str> {
        self.summary
            .as_deref()
            .or(self.title.as_deref())
            .or(self.cwd.as_deref())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEventEntry {
    /// Replaces everything the client holds.
    Snapshot(Vec<AgentEntry>),
    /// Inserts the agent, or replaces the row with its pane id.
    Changed(Box<AgentEntry>),
    Removed(u32),
}

/// [`index`](Self::index) is a display position that shifts when an earlier tab closes,
/// [`key`](Self::key) is this screen's lookup handle, [`tab_id`](Self::tab_id) the backend's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabEntry {
    pub index: u32,
    pub key: String,
    pub tab_id: u64,
    pub label: String,
    pub pane_title: String,
    pub pane_name: String,
    pub pane_path: String,
    pub panes: Vec<PaneEntry>,
    pub active: bool,
    pub has_bell: bool,
    /// Floating panes are hit-testable only while true.
    pub floating_panes_visible: bool,
    /// Tab-global and client-independent.
    pub fullscreen_active: bool,
}

impl TabEntry {
    pub fn pill_label(&self) -> String {
        format!("{} {}", self.index, self.label)
    }
}

/// The wire's `ScrollDirection` has more steps; this screen sends only wheel notches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollStep {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermLineKind {
    Prompt,
    Output,
    Comment,
}

/// The prompt marker is part of the text; there is no separate marker field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermLine {
    pub kind: TermLineKind,
    pub text: String,
    pub cursor: bool,
}

impl TermLine {
    pub fn prompt(text: impl Into<String>) -> Self {
        Self {
            kind: TermLineKind::Prompt,
            text: text.into(),
            cursor: false,
        }
    }

    pub fn caret(text: impl Into<String>) -> Self {
        Self {
            kind: TermLineKind::Prompt,
            text: text.into(),
            cursor: true,
        }
    }

    pub fn output(text: impl Into<String>) -> Self {
        Self {
            kind: TermLineKind::Output,
            text: text.into(),
            cursor: false,
        }
    }

    pub fn comment(text: impl Into<String>) -> Self {
        Self {
            kind: TermLineKind::Comment,
            text: text.into(),
            cursor: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Workspace {
    pub spaces: Vec<Space>,
    pub tabs: Vec<TabEntry>,
    pub lines: Vec<TermLine>,
    pub session_name: Option<String>,
}

impl Workspace {
    pub fn active_key(&self) -> Option<String> {
        self.tabs
            .iter()
            .find(|tab| tab.active)
            .or_else(|| self.tabs.first())
            .map(|tab| tab.key.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_pill_prints_its_index_and_label() {
        let tab = TabEntry {
            index: 4,
            tab_id: 4,
            key: "devbox".to_string(),
            label: "devbox".to_string(),
            pane_title: "ed@devbox: ~".to_string(),
            pane_name: "ed@devbox".to_string(),
            pane_path: "/home/ed".to_string(),
            panes: Vec::new(),
            active: true,
            has_bell: false,
            floating_panes_visible: false,
            fullscreen_active: false,
        };
        assert_eq!(tab.pill_label(), "4 devbox");
    }

    #[test]
    fn only_the_caret_constructor_carries_a_cursor() {
        assert!(!TermLine::prompt("x").cursor);
        assert!(!TermLine::output("x").cursor);
        assert!(!TermLine::comment("x").cursor);
        assert!(TermLine::caret("x").cursor);
    }

    #[test]
    fn each_constructor_keeps_its_own_ink_tier() {
        assert_eq!(TermLine::prompt("x").kind, TermLineKind::Prompt);
        assert_eq!(TermLine::output("x").kind, TermLineKind::Output);
        assert_eq!(TermLine::comment("x").kind, TermLineKind::Comment);
    }

    #[test]
    fn the_active_key_falls_back_to_the_first_tab() {
        let tab = |key: &str, active: bool| TabEntry {
            index: 1,
            tab_id: 1,
            key: key.to_string(),
            label: key.to_string(),
            pane_title: String::new(),
            pane_name: String::new(),
            pane_path: String::new(),
            panes: Vec::new(),
            active,
            has_bell: false,
            floating_panes_visible: false,
            fullscreen_active: false,
        };
        let mut workspace = Workspace {
            tabs: vec![tab("a", false), tab("b", true)],
            ..Workspace::default()
        };
        assert_eq!(workspace.active_key().as_deref(), Some("b"));

        workspace.tabs[1].active = false;
        assert_eq!(
            workspace.active_key().as_deref(),
            Some("a"),
            "with nothing marked active the card still shows a tab"
        );

        assert_eq!(Workspace::default().active_key(), None);
    }

    #[test]
    fn a_tabs_pane_list_carries_every_pane_it_was_built_with() {
        let pane = PaneEntry {
            agent: None,
            id: 3,
            title: "zsh".to_string(),
            cwd: "/srv".to_string(),
            focused: true,
            floating: false,
            exited: false,
            is_plugin: false,
            is_fullscreen: false,
            x: 0,
            y: 0,
            rows: 24,
            cols: 80,
        };
        let tab = TabEntry {
            index: 1,
            tab_id: 1,
            key: "devbox".to_string(),
            label: "devbox".to_string(),
            pane_title: String::new(),
            pane_name: String::new(),
            pane_path: String::new(),
            panes: vec![pane.clone()],
            active: true,
            has_bell: true,
            floating_panes_visible: false,
            fullscreen_active: false,
        };
        assert_eq!(tab.panes, vec![pane]);
        assert!(tab.has_bell);
    }

    #[test]
    fn scroll_step_has_exactly_two_wheel_notch_values() {
        assert_ne!(ScrollStep::Up, ScrollStep::Down);
    }
}
