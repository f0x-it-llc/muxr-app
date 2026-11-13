//! `terminal` domain entities — plain-data types mirroring `docs/terminal.html` v2 and the mock's
//! `TABS` map: [`Space`] (the `spaces` pill row), [`TabEntry`]/[`PaneEntry`] (one `TABS` entry),
//! [`TermLine`]/[`TermLineKind`] (one `.term-line`) and [`Workspace`] (one load).
//!
//! The mock world is session-agnostic: content is fixed regardless of the attached session id,
//! except [`Workspace::session_name`] (the rename dialog's seed).
//!
//! [`RenameRequest`] and [`SettingsCursorStyle`] are re-exported from `sessions`/`settings` rather
//! than re-minted — a session's name and a cursor shape each have exactly one owner.
//! `SettingsCursorStyle` carries the qualified name to stay unambiguous beside
//! `presentation::term::CursorStyle`, the grid's own type the two map between.

pub use crate::agents::AgentStatus;
pub use crate::features::sessions::domain::entities::RenameRequest;
pub use crate::features::settings::domain::entities::CursorStyle as SettingsCursorStyle;

/// One zellij *space* — the sheet's `spaces` pill row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Space {
    /// The opaque backend id a switch addresses this space by (the wire's `Space.id`), never
    /// shown to the user and separate from [`name`](Self::name): a space can be renamed without
    /// its id moving. The in-memory mock has no backend, so its ids are its names.
    pub id: String,
    /// The pill's label (`forgekit`).
    pub name: String,
    /// Whether this is the selected pill (`.pill.sel`). Exactly one space is
    /// selected in the mock.
    pub selected: bool,
}

/// One pane inside a [`TabEntry`] — mirrors the wire's `PaneMsg`, carrying only what the app
/// consumes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneEntry {
    /// The wire's `PaneMsg.id` — unique within its own [`is_plugin`](Self::is_plugin) namespace,
    /// never across the two.
    pub id: u32,
    pub title: String,
    /// The pane's current working directory, when the backend reports one.
    pub cwd: String,
    /// Whether this pane is focused within its layer (tiled/floating).
    pub focused: bool,
    pub floating: bool,
    /// Whether the pane's command has exited (command panes only).
    pub exited: bool,
    /// Whether this is a plugin pane rather than a terminal one — picks the wire's
    /// `PaneId::Plugin` vs `PaneId::Terminal` namespace for pane-targeted verbs.
    pub is_plugin: bool,
    /// zellij flags EVERY client-active pane while the tab is fullscreen (union `is_focused`); the
    /// tab truth is [`TabEntry::fullscreen_active`], the fullscreened pane is the focused one.
    pub is_fullscreen: bool,
    /// Geometry, in cells: left edge, top edge, height, width.
    pub x: u32,
    pub y: u32,
    pub rows: u32,
    pub cols: u32,
    /// The AI agent running in this pane, when the backend reports one (the wire's
    /// `PaneMsg.agent`). `None` on a backend without an agent concept.
    pub agent: Option<AgentMark>,
}

/// The agent riding on a [`PaneEntry`] — the wire's `AgentMark`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMark {
    /// The agent's CLI label as the backend reports it.
    pub kind: String,
    /// The user-facing agent name, when the backend has one.
    pub name: Option<String>,
    pub status: AgentStatus,
}

/// One agent in the session-wide inventory (the wire's `AgentMsg`), flattened so a row can
/// jump to its pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEntry {
    pub pane_id: u32,
    pub tab_id: u64,
    /// The same key [`TabEntry::key`] carries for this tab, so a row can address
    /// `commit_pane_in_space`. Empty when the tab is not in its space's layout.
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
    /// The row's headline: the agent's name, else its human kind, else the raw kind.
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.display_kind.as_deref())
            .unwrap_or(&self.kind)
    }

    /// The row's second line: the activity summary, else the title, else the cwd.
    pub fn subtitle(&self) -> Option<&str> {
        self.summary
            .as_deref()
            .or(self.title.as_deref())
            .or(self.cwd.as_deref())
    }
}

/// One message of a live agents subscription (the wire's `AgentEvent`), mapped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEventEntry {
    /// The whole inventory, replacing everything the client holds.
    Snapshot(Vec<AgentEntry>),
    /// One agent's complete current state: insert it, or replace the row with its pane id.
    /// Boxed, so the enum stays the size of its small variants.
    Changed(Box<AgentEntry>),
    /// The agent in this pane is gone.
    Removed(u32),
}

/// One tab in the attached session — a `TABS` entry, whole. Three identifiers ride on it and
/// none derives from another: [`index`](Self::index) is a display position that shifts when an
/// earlier tab closes, [`key`](Self::key) is this screen's own lookup handle, and
/// [`tab_id`](Self::tab_id) is the routing key the backend knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabEntry {
    /// The tab's 1-based position, as the pill prints it.
    pub index: u32,
    /// The mock's `TABS` key — the sheet's `panes — <key>` section label, and what
    /// [`SwitchTab`](super::use_cases::SwitchTab) addresses by.
    pub key: String,
    /// The tab's stable backend id (the wire's `TabMsg.tab_id`) — what a *switch* addresses it
    /// by; see the struct docs above for why it's distinct from `index`/`key`.
    pub tab_id: u64,
    /// The pill's human label (`lumen-api`).
    pub label: String,
    /// The card status row's pane title (`ed@devbox: ~`).
    pub pane_title: String,
    /// The focused pane's name (`ed@devbox`) — mock-era convenience, carried by every
    /// repository; nothing on screen reads it today.
    pub pane_name: String,
    /// The focused pane's path (`/home/ed`) — same standing as [`pane_name`](Self::pane_name).
    pub pane_path: String,
    /// Every pane in this tab, in wire order — the "panes & tabs" sheet's pane list. Separate
    /// from the `pane_*` fields, which stay the focused-pane convenience every existing caller
    /// still reads.
    pub panes: Vec<PaneEntry>,
    /// Whether this is the tab the card shows on arrival (`devbox`).
    pub active: bool,
    /// Whether the tab carries an unacknowledged bell — drawn only by the docked rail.
    pub has_bell: bool,
    /// The wire's `TabMsg.floating_panes_visible`; floating panes are hit-testable only while true.
    pub floating_panes_visible: bool,
    /// The wire's `TabMsg.fullscreen_active`: tab-global and client-independent.
    pub fullscreen_active: bool,
}

impl TabEntry {
    /// The sheet pill's label: `"4 devbox"`.
    pub fn pill_label(&self) -> String {
        format!("{} {}", self.index, self.label)
    }
}

/// Wheel-scroll granularity
/// [`TerminalRepository::scroll_pane`](super::repositories::TerminalRepository::scroll_pane)
/// sends. The wire's `ScrollDirection` also carries page/half-page/top/bottom, but nothing on
/// this screen drives more than a mouse-wheel notch yet (`docs/ARCHITECTURE.md`'s Planned
/// section), so this enum names only the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollStep {
    Up,
    Down,
}

/// Which kind of line a [`TermLine`] is — each has its own ink (the mock's
/// `.p`/`.out`/`.cm` spans).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermLineKind {
    /// A command line, amber (`GlyphInk::terminal_prompt`).
    Prompt,
    /// Command output, dimmer (`GlyphInk::terminal_out`).
    Output,
    /// A comment/annotation, dimmest and *italic* (`GlyphInk::terminal_comment`).
    Comment,
}

/// One line of the terminal card's body. The mock writes each line's prompt marker into the
/// text itself (`ed@devbox:~$ systemctl status muxrd`) — no separate marker field, unlike
/// `frust_glyph::term_block`, which prepends `$ `.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermLine {
    pub kind: TermLineKind,
    pub text: String,
    /// Whether the blinking block caret sits at the end of this line — the mock's
    /// `<span class="cur">`, which only follows the trailing empty prompt.
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

    /// A command line ending in the blinking caret.
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

/// Everything one load of the terminal screen needs: the sheet's two pill rows, the card's
/// current lines, and the attached session's name. One aggregate rather than four calls because
/// the screen has one loading state — and the active tab (which decides which lines come back)
/// is the repository's answer.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Workspace {
    pub spaces: Vec<Space>,
    pub tabs: Vec<TabEntry>,
    /// The [active](TabEntry::active) tab's lines.
    pub lines: Vec<TermLine>,
    /// The attached session's current name, when `/terminal` was reached with a `?session=<id>`
    /// that still resolves — see the [module docs](self).
    pub session_name: Option<String>,
}

impl Workspace {
    /// The key of the tab marked [active](TabEntry::active), or the first tab's —
    /// the card has to show *something* even if a repository forgets to mark one.
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
