//! `terminal` domain entities — the feature's plain-data types
//! (framework-free, per `crate::features::terminal::domain`'s module doc).
//!
//! Every type here exists because `docs/terminal.html` (**v2**, "One less
//! row") paints it, and each carries exactly the fields that mock's own
//! `TABS` map and sheet markup supply — nothing speculative. A real PTY frame,
//! a scrollback buffer, a pane tree: none of those are here, because none of
//! them is on screen yet.
//!
//! | Type | Mock element |
//! |---|---|
//! | [`Space`] | the sheet's `spaces` pill row (`cupline`/`zelli_mobile`/`forgekit`) |
//! | [`TabEntry`]/[`PaneEntry`] | one `TABS` entry — its pill (`4 devbox`), the card's pane title, and the sheet's own pane rows |
//! | [`TermLine`]/[`TermLineKind`] | one `.term-line` in the card's body, typed `p`/`out`/`cm` |
//! | [`Workspace`] | one load: every space, every tab, the active tab's lines, and the attached session's name |
//!
//! # The mock world is session-agnostic, deliberately
//!
//! `/terminal?session=<id>` carries the session the user attached to, but
//! `docs/terminal.html`'s content is **fixed** regardless of which session was
//! tapped — so nothing below is keyed by session id. The one place the id
//! shows up is [`Workspace::session_name`], which exists only so the overflow
//! menu's *Rename session* dialog can seed itself with the attached session's
//! current name (and write it back through the `sessions` feature's own
//! repository). When real `muxrd` tab/pane state lands, the repository starts
//! taking the id and nothing above this file changes shape.
//!
//! # Session-rename reuse (a documented layering tension)
//!
//! [`RenameRequest`] is **not** re-minted here: it is
//! [`crate::features::sessions::domain::entities::RenameRequest`], the
//! `sessions` feature's own request shape for its one rename write path — a
//! session's *name* has exactly one owner (the `sessions` list and its
//! store), so this screen's rename dialog fills the very struct that store
//! reads rather than a second, driftable one. See
//! `crate::features::terminal::domain`'s module docs for the full accounting
//! of this `domain` → sibling `domain` widening (the same shape `connect`'s
//! own `TrustMode` re-export uses); nothing below crosses into `sessions`'
//! `data` or `presentation`, and `presentation::controller` reaches this type
//! through here rather than naming `sessions::domain` itself.
//!
//! # Cursor-style reuse (a documented layering tension) — the third widening
//!
//! [`SettingsCursorStyle`] is likewise **not** re-minted here: it is the
//! `settings` feature's own terminal-appearance cursor shape, re-exported
//! under this qualified name (rather than bare `CursorStyle`) so it stays
//! unambiguous beside `presentation::term::CursorStyle`, the live grid's own
//! cursor-shape type the two are mapped between. A cursor shape has exactly
//! one owner (the settings screen's own picker and its store), so the live
//! pane's own cursor-style mapping fn reuses it rather than a second,
//! driftable enum. Same accounting as the rename widening above
//! (`crate::features::terminal::domain`'s module docs record it as the third
//! use of this app's `domain` → sibling `domain` widening); nothing below
//! crosses into `settings`' `data` or
//! `presentation`, and `presentation::controller` reaches this type through
//! here rather than naming a sibling's `domain` itself.

pub use crate::features::sessions::domain::entities::RenameRequest;
pub use crate::features::settings::domain::entities::CursorStyle as SettingsCursorStyle;

/// One zellij *space* — the sheet's `spaces` pill row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Space {
    /// The opaque backend id a switch addresses this space by — the wire's own
    /// `Space.id`, never shown to the user.
    ///
    /// Separate from [`name`](Self::name) because the two are separate on the
    /// wire: a space can be renamed without its id moving, and a display name
    /// is not unique enough to route on. The in-memory mock has no backend
    /// behind it, so its ids are its names.
    pub id: String,
    /// The pill's label (`forgekit`).
    pub name: String,
    /// Whether this is the selected pill (`.pill.sel`). Exactly one space is
    /// selected in the mock.
    pub selected: bool,
}

/// One pane inside a [`TabEntry`] — mirrors the wire's `PaneMsg`
/// (`proto/muxr.proto`), carrying only what the app consumes: no wire-only
/// bookkeeping (`command`) and nothing this screen has no affordance to show
/// yet. The four geometry fields are cells, matching every other geometry
/// value in this app (the wire's own `x`/`y`/`rows`/`cols`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneEntry {
    /// The wire's `PaneMsg.id` — unique within its own
    /// [`is_plugin`](Self::is_plugin) namespace, never across the two (a
    /// terminal pane and a plugin pane can share an id).
    pub id: u32,
    /// The pane's display title.
    pub title: String,
    /// The pane's current working directory, when the backend reports one.
    pub cwd: String,
    /// Whether this pane is focused within its layer (tiled/floating).
    pub focused: bool,
    /// Whether this pane floats rather than tiles.
    pub floating: bool,
    /// Whether the pane's command has exited (command panes only).
    pub exited: bool,
    /// Whether this is a plugin pane rather than a terminal one — the flag
    /// [`TerminalRepository`](super::repositories::TerminalRepository)'s
    /// pane-targeted verbs need to pick the wire's `PaneId::Plugin` vs
    /// `PaneId::Terminal` namespace.
    pub is_plugin: bool,
    /// Whether this pane is currently fullscreened.
    pub is_fullscreen: bool,
    /// Geometry: left edge, in cells.
    pub x: u32,
    /// Geometry: top edge, in cells.
    pub y: u32,
    /// Geometry: height, in cells.
    pub rows: u32,
    /// Geometry: width, in cells.
    pub cols: u32,
}

/// One tab in the attached session — a `TABS` entry, whole.
///
/// The mock's map is keyed by [`key`](Self::key) (`devbox`) while the pill
/// prints `"<index> <label>"` (`4 devbox`) and the sheet's pane section prints
/// the bare key, so all three are modelled separately rather than derived from
/// each other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabEntry {
    /// The tab's 1-based position, as the pill prints it.
    pub index: u32,
    /// The mock's `TABS` key (`btop`, `minefit`, `gohora`, `devbox`) — what
    /// the sheet's `panes — <key>` section label prints, and what
    /// [`SwitchTab`](super::use_cases::SwitchTab) addresses a tab by.
    pub key: String,
    /// The tab's stable backend id — what a *switch* addresses it by.
    ///
    /// Three separate identifiers ride on one tab, and none derives from
    /// another: [`index`](Self::index) is a display position that shifts the
    /// moment a tab before it closes, [`key`](Self::key) is this screen's own
    /// display/lookup handle, and this is the routing key the backend knows
    /// (the wire's `TabMsg.tab_id`). The in-memory mock has no backend behind
    /// it, so its ids are its positions.
    pub tab_id: u64,
    /// The pill's human label (`minefit-api`).
    pub label: String,
    /// The card status row's pane title (`ed@devbox: ~`).
    pub pane_title: String,
    /// The focused pane's name (`ed@devbox`) — the mock era's own convenience,
    /// carried by every repository. The sheet lists [`panes`](Self::panes)
    /// instead, so nothing on screen reads this today.
    pub pane_name: String,
    /// The focused pane's path (`/home/ed`) — same standing as
    /// [`pane_name`](Self::pane_name).
    pub pane_path: String,
    /// Every pane in this tab, in wire order — the "panes & tabs" sheet's own
    /// pane list. Separate from [`pane_title`](Self::pane_title)/
    /// [`pane_name`](Self::pane_name)/[`pane_path`](Self::pane_path), which
    /// stay the *focused*-pane convenience the mock era minted and every
    /// existing caller still reads; this field is additive, so nothing above
    /// this file changes shape.
    pub panes: Vec<PaneEntry>,
    /// Whether this is the tab the card shows on arrival (`devbox`).
    pub active: bool,
}

impl TabEntry {
    /// The sheet pill's label: `"4 devbox"`.
    pub fn pill_label(&self) -> String {
        format!("{} {}", self.index, self.label)
    }
}

/// Wheel-scroll granularity
/// [`TerminalRepository::scroll_pane`](super::repositories::TerminalRepository::scroll_pane)
/// sends.
///
/// The wire's `ScrollDirection` also carries page/half-page/top/bottom, but
/// nothing on this screen drives anything but a mouse-wheel notch yet (see
/// `docs/ARCHITECTURE.md`'s Planned section) — so this domain enum only names
/// the two a wheel actually produces, rather than pre-minting five verbs
/// nothing calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollStep {
    /// One wheel notch up.
    Up,
    /// One wheel notch down.
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

/// One line of the terminal card's body.
///
/// The mock writes each line's prompt marker into the text itself
/// (`ed@devbox:~$ systemctl status muxrd`), so there is no separate marker
/// field — unlike `frust::glyph::term_block`, which prepends its own `$ `.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermLine {
    /// Which ink this line prints in.
    pub kind: TermLineKind,
    /// The line's text, verbatim.
    pub text: String,
    /// Whether the blinking block caret sits at the end of this line — the
    /// mock's `<span class="cur">`, which only ever follows the trailing empty
    /// prompt.
    pub cursor: bool,
}

impl TermLine {
    /// A command line.
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

    /// A command-output line.
    pub fn output(text: impl Into<String>) -> Self {
        Self {
            kind: TermLineKind::Output,
            text: text.into(),
            cursor: false,
        }
    }

    /// A comment/annotation line.
    pub fn comment(text: impl Into<String>) -> Self {
        Self {
            kind: TermLineKind::Comment,
            text: text.into(),
            cursor: false,
        }
    }
}

/// Everything one load of the terminal screen needs: the sheet's two pill
/// rows, the card's current lines, and the attached session's name.
///
/// One aggregate rather than four calls because the screen has one loading
/// state, not four — and because the *active* tab (which decides which lines
/// come back) is the repository's answer, not the screen's guess.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Workspace {
    /// Every space, in the order the sheet draws them.
    pub spaces: Vec<Space>,
    /// Every tab, in the order the sheet draws them.
    pub tabs: Vec<TabEntry>,
    /// The [active](TabEntry::active) tab's lines.
    pub lines: Vec<TermLine>,
    /// The attached session's current name, when `/terminal` was reached with
    /// a `?session=<id>` that still resolves — see the [module docs](self).
    pub session_name: Option<String>,
}

impl Workspace {
    /// The key of the tab marked [active](TabEntry::active), or the first
    /// tab's — the card has to show *something* even if a future repository
    /// forgets to mark one.
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
        };
        assert_eq!(tab.panes, vec![pane]);
    }

    #[test]
    fn scroll_step_has_exactly_two_wheel_notch_values() {
        assert_ne!(ScrollStep::Up, ScrollStep::Down);
    }
}
