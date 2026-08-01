//! [`TerminalController`] — the `/terminal` screen's view model.
//!
//! Mirrors the shape `docs/ARCHITECTURE.md`'s **Building a Screen** section
//! mandates and `features::sessions`' own controller establishes: a
//! [`ControllerCore`] embedded by composition, the feature's four use cases
//! constructed over two injected repositories, and the signals the screen
//! renders. Hosted by the screen through `clean_signals_frust::use_controller`,
//! so it is created on route mount and disposed on unmount.
//!
//! # Signals vs. plain state, and where the *animations* live
//!
//! Everything below is observed state a rebuild depends on: the tab world, the
//! active tab, its lines, whether the screen is fullscreen. Writing one wakes
//! the screen's rebuild, which is exactly what each is for.
//!
//! What is deliberately **not** here is per-frame animation progress. The
//! card's staggered line reveal, its blinking caret and the fullscreen
//! clearance/padding/radius transition are all advanced from `Widget::paint`
//! against the shell's frame clock (`crate::features::terminal::presentation::chrome`),
//! because a signal written once per frame would wake a whole-app rebuild per
//! frame — the one thing `docs/ARCHITECTURE.md`'s rebuild model rules out.
//! The controller instead publishes the *cue*: [`reveal_epoch`](Self::reveal_epoch)
//! is a monotonic replay token, bumped whenever the reveal should start over,
//! and the body widget restarts its own driver when the number it was handed
//! changes. One rebuild per replay; zero per frame.
//!
//! # Fullscreen is controller state, not widget state
//!
//! [`fullscreen`](Self::fullscreen) drives three separate things — the card's
//! own geometry, the expand button's on-state and icon, and the *shell bar's*
//! hidden flag (published through `crate::routes`) — so it cannot live inside
//! any one widget. It also has to survive the sheet/menu pages pushed over the
//! screen, which is another way of saying it belongs to the screen's view
//! model.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clean_signals::{ControllerCore, RunOptions};
use frust::{GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::RenameRequest;
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::features::terminal::domain::entities::{Space, TabEntry, TermLine, Workspace};
use crate::features::terminal::domain::repositories::TerminalRepository;
use crate::features::terminal::domain::use_cases::{
    DetachSession, LoadWorkspace, RenameAttachedSession, SwitchTab,
};

/// The `/terminal` screen's view model. See the [module docs](self).
pub struct TerminalController {
    core: ControllerCore<AppFailure>,
    load: LoadWorkspace,
    switch: SwitchTab,
    rename: RenameAttachedSession,
    detach: DetachSession,
    /// The attached session's id (`/terminal?session=<id>`), or `None` for a
    /// bare `/terminal`. Plain data: it is fixed for this mount.
    session_id: Option<String>,
    /// The sheet's `spaces` pill row.
    pub spaces: RwSignal<Vec<Space>>,
    /// The sheet's `tabs` pill row.
    pub tabs: RwSignal<Vec<TabEntry>>,
    /// The key of the tab the card is showing.
    pub active_tab: RwSignal<String>,
    /// The active tab's lines — what the card's body prints.
    pub lines: RwSignal<Vec<TermLine>>,
    /// The attached session's current name, for the rename dialog's seed.
    pub session_name: RwSignal<Option<String>>,
    /// Whether the first load has landed (the card renders an empty body
    /// rather than a wrong one until it has).
    pub loaded: RwSignal<bool>,
    /// Whether the card is fullscreen — see the [module docs](self).
    pub fullscreen: RwSignal<bool>,
    /// The reveal replay token — see the [module docs](self).
    pub reveal_epoch: RwSignal<u64>,
    /// Mints [`reveal_epoch`](Self::reveal_epoch) values. An atomic rather
    /// than a read-modify-write on the signal itself, so a bump made from a
    /// press handler (no reactive owner on desktop) never reads a signal it
    /// is about to write.
    next_epoch: AtomicU64,
}

impl TerminalController {
    /// A fresh controller over the injected repositories (the composition root
    /// built each once — see [`crate::features::terminal::di`] and
    /// [`crate::features::sessions::di`]), attached to `session_id`.
    pub fn new(
        terminal: Arc<dyn TerminalRepository + Send + Sync>,
        sessions: Arc<dyn SessionsRepository + Send + Sync>,
        session_id: Option<String>,
    ) -> Self {
        Self {
            core: ControllerCore::new(),
            load: LoadWorkspace::new(Arc::clone(&terminal), Arc::clone(&sessions)),
            switch: SwitchTab::new(Arc::clone(&terminal)),
            rename: RenameAttachedSession::new(sessions),
            detach: DetachSession::new(terminal),
            session_id,
            spaces: RwSignal::new(Vec::new()),
            tabs: RwSignal::new(Vec::new()),
            active_tab: RwSignal::new(String::new()),
            lines: RwSignal::new(Vec::new()),
            session_name: RwSignal::new(None),
            loaded: RwSignal::new(false),
            fullscreen: RwSignal::new(false),
            reveal_epoch: RwSignal::new(0),
            next_epoch: AtomicU64::new(1),
        }
    }

    /// This controller's [`ControllerCore`] — the failure sink the screen
    /// subscribes its toast bridge to (`use_failure_listener`). A named
    /// accessor rather than the [`AsRef`] impl below because
    /// `Arc<TerminalController>` has its own inherent `as_ref`, which would
    /// make the bare call ambiguous at every site.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// The attached session's id, if this screen was reached with one.
    pub fn session_id(&self) -> Option<String> {
        self.session_id.clone()
    }

    /// Load the whole screen (spaces, tabs, the active tab's lines, the
    /// attached session's name) and play the reveal.
    pub async fn load(&self) {
        let Ok(workspace) = self
            .core
            .run(&self.load, self.session_id.clone(), RunOptions::default())
            .await
        else {
            return;
        };
        let Workspace {
            spaces,
            tabs,
            lines,
            session_name,
        } = workspace;
        let active = tabs
            .iter()
            .find(|tab| tab.active)
            .or_else(|| tabs.first())
            .map(|tab| tab.key.clone())
            .unwrap_or_default();
        self.spaces.set(spaces);
        self.tabs.set(tabs);
        self.active_tab.set(active);
        self.lines.set(lines);
        self.session_name.set(session_name);
        self.loaded.set(true);
        self.replay_reveal();
    }

    /// Switch the card to the tab keyed `key`: its lines replace the body's
    /// and the reveal replays from the top (the mock's own behaviour —
    /// `renderTab` ends in `playReveal`).
    ///
    /// Switching to the tab already showing still replays, which is what makes
    /// this the one path a "replay" affordance would reuse.
    pub async fn switch_tab(&self, key: String) {
        let Ok(lines) = self
            .core
            .run(&self.switch, key.clone(), RunOptions::default())
            .await
        else {
            return;
        };
        self.active_tab.set(key.clone());
        self.tabs.update_active(&key);
        self.lines.set(lines);
        self.replay_reveal();
    }

    /// Select the space named `name` — selection state only, in the mock era
    /// (nothing else on screen depends on which space is current).
    pub fn select_space(&self, name: &str) {
        let spaces = self
            .spaces
            .get_untracked()
            .into_iter()
            .map(|space| Space {
                selected: space.name == name,
                ..space
            })
            .collect();
        self.spaces.set(spaces);
    }

    /// Rename the attached session to `name` (already trimmed by the dialog),
    /// writing through the **sessions** repository so the list reflects it on
    /// the way back. Answers the stored name it landed on, for the toast.
    pub async fn rename(&self, name: String) -> Option<String> {
        let id = self.session_id.clone()?;
        let session = self
            .core
            .run(
                &self.rename,
                RenameRequest { id, name },
                RunOptions::default(),
            )
            .await
            .ok()?;
        self.session_name.set(Some(session.name.clone()));
        Some(session.name)
    }

    /// Detach from the attached session. Answers whether the call landed, so
    /// the caller only pops (and toasts) on success.
    pub async fn detach(&self) -> bool {
        let Some(id) = self.session_id.clone() else {
            return false;
        };
        self.core
            .run(&self.detach, id, RunOptions::default())
            .await
            .is_ok()
    }

    /// Flip fullscreen and answer the new state — the expand button's whole
    /// behaviour (the caller publishes it to the shell bar).
    pub fn toggle_fullscreen(&self) -> bool {
        let next = !self.fullscreen.get_untracked();
        self.fullscreen.set(next);
        next
    }

    /// The tab the card is showing, if the load has landed.
    pub fn active_tab_entry(&self) -> Option<TabEntry> {
        let key = self.active_tab.get_untracked();
        self.tabs
            .get_untracked()
            .into_iter()
            .find(|tab| tab.key == key)
    }

    /// Bump the reveal replay token — see the [module docs](self).
    fn replay_reveal(&self) {
        self.reveal_epoch
            .set(self.next_epoch.fetch_add(1, Ordering::Relaxed));
    }
}

/// Re-flagging the active tab in place, so the sheet's pills agree with the
/// card without a second load.
trait ActiveTab {
    /// Mark `key`'s tab active and every other tab not.
    fn update_active(&self, key: &str);
}

impl ActiveTab for RwSignal<Vec<TabEntry>> {
    fn update_active(&self, key: &str) {
        let tabs = self
            .get_untracked()
            .into_iter()
            .map(|tab| TabEntry {
                active: tab.key == key,
                ..tab
            })
            .collect();
        self.set(tabs);
    }
}

impl AsRef<ControllerCore<AppFailure>> for TerminalController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::sessions::data::repositories::MockSessionsRepository;
    use crate::features::terminal::data::repositories::MockTerminalRepository;

    /// The test-wiring composition root: both mock repositories behind their
    /// domain traits. `#[cfg(test)]`-only wiring, so `di.rs` stays the only
    /// production site where presentation meets `data` (huddle's convention).
    fn controller(session_id: Option<&str>) -> TerminalController {
        TerminalController::new(
            Arc::new(MockTerminalRepository::seeded()),
            Arc::new(MockSessionsRepository::seeded()),
            session_id.map(str::to_string),
        )
    }

    /// Drive a controller call to completion on a current-thread runtime.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn a_load_lands_on_the_mocks_active_tab() {
        let c = controller(Some("s-1"));
        assert!(!c.loaded.get_untracked());

        block_on(c.load());

        assert!(c.loaded.get_untracked());
        assert_eq!(c.spaces.get_untracked().len(), 3);
        assert_eq!(c.tabs.get_untracked().len(), 4);
        assert_eq!(c.active_tab.get_untracked(), "devbox");
        assert_eq!(
            c.active_tab_entry().map(|t| t.pane_title),
            Some("ed@devbox: ~".to_string())
        );
        assert_eq!(
            c.lines.get_untracked().first().map(|l| l.text.clone()),
            Some("ed@devbox:~$ systemctl status muxrd".to_string())
        );
        assert_eq!(c.session_name.get_untracked().as_deref(), Some("dev"));
    }

    #[test]
    fn switching_a_tab_swaps_the_lines_and_replays_the_reveal() {
        let c = controller(None);
        block_on(c.load());
        let before = c.reveal_epoch.get_untracked();

        block_on(c.switch_tab("btop".to_string()));

        assert_eq!(c.active_tab.get_untracked(), "btop");
        assert_eq!(
            c.lines.get_untracked().first().map(|l| l.text.clone()),
            Some("ed@precision:~$ btop".to_string())
        );
        assert!(
            c.reveal_epoch.get_untracked() > before,
            "the replay token advances on every switch"
        );
        // …and the sheet's pills follow the card.
        let active: Vec<String> = c
            .tabs
            .get_untracked()
            .into_iter()
            .filter(|t| t.active)
            .map(|t| t.key)
            .collect();
        assert_eq!(active, ["btop"]);
    }

    #[test]
    fn switching_to_an_unknown_tab_changes_nothing() {
        let c = controller(None);
        block_on(c.load());
        let epoch = c.reveal_epoch.get_untracked();

        block_on(c.switch_tab("nope".to_string()));

        assert_eq!(c.active_tab.get_untracked(), "devbox");
        assert_eq!(
            c.reveal_epoch.get_untracked(),
            epoch,
            "no replay on failure"
        );
    }

    #[test]
    fn selecting_a_space_moves_the_selection() {
        let c = controller(None);
        block_on(c.load());

        c.select_space("cupline");

        let selected: Vec<String> = c
            .spaces
            .get_untracked()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.name)
            .collect();
        assert_eq!(selected, ["cupline"]);
    }

    #[test]
    fn a_rename_round_trips_through_the_sessions_repository() {
        let sessions = Arc::new(MockSessionsRepository::seeded());
        let c = TerminalController::new(
            Arc::new(MockTerminalRepository::seeded()),
            Arc::clone(&sessions) as Arc<dyn SessionsRepository + Send + Sync>,
            Some("s-1".to_string()),
        );
        block_on(c.load());

        assert_eq!(
            block_on(c.rename("dev-2".to_string())).as_deref(),
            Some("dev-2")
        );

        assert_eq!(c.session_name.get_untracked().as_deref(), Some("dev-2"));
        let names: Vec<String> = sessions.snapshot().into_iter().map(|s| s.name).collect();
        assert!(
            names.contains(&"dev-2".to_string()),
            "the sessions list sees it too: {names:?}"
        );
    }

    #[test]
    fn a_rename_without_an_attached_session_is_a_no_op() {
        let c = controller(None);
        block_on(c.load());
        assert_eq!(block_on(c.rename("whatever".to_string())), None);
        assert_eq!(c.session_name.get_untracked(), None);
    }

    #[test]
    fn detach_needs_an_attached_session() {
        let attached = controller(Some("s-1"));
        assert!(block_on(attached.detach()));

        let bare = controller(None);
        assert!(!block_on(bare.detach()), "nothing to detach from");
    }

    #[test]
    fn fullscreen_toggles_both_ways() {
        let c = controller(None);
        assert!(!c.fullscreen.get_untracked());
        assert!(c.toggle_fullscreen());
        assert!(c.fullscreen.get_untracked());
        assert!(!c.toggle_fullscreen());
        assert!(!c.fullscreen.get_untracked());
    }
}
