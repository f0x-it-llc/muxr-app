//! [`MockTerminalRepository`] — the in-memory [`TerminalRepository`] every headless suite and
//! the mock-era screen run on.
//!
//! Seeds exactly what `docs/terminal.html` v2 ships — three spaces, four tabs, each tab's
//! title/name/path and its four `.term-line`s, transcribed rather than paraphrased. Framework-free:
//! a [`Mutex`]-guarded space list plus immutable tab data; the mutex (not a `RefCell`) is what
//! makes it `Send + Sync`, which the controller needs to hold it behind an `Arc`.
//!
//! **What mutates**: space selection, and pane focus — [`TerminalRepository::focus_pane`] moves
//! the `focused` flag, so a later `list_tabs` agrees with the move a client just made. The tab
//! set, its panes' identities and its lines are fixed.
//!
//! **Headless-test tab seam.** A tab world that never moves cannot exercise the screen's reason
//! for re-reading one, so [`add_test_tab`]/[`clear_test_tabs`] append synthetic tabs to every
//! later `list_tabs`. Process-global — belongs to a serialized suite, since a suite reaches this
//! store only as the `Arc<dyn TerminalRepository>` its `di` published.
//!
//! Failure mapping: an unknown tab key or blank session id maps to [`AppFailure::Internal`] — a
//! bug, since the screen only addresses what it just rendered. [`MOCK_LATENCY`] is a short sleep
//! before every call, so the screen's async path is the one the real client will take.

use std::sync::Mutex;
use std::time::Duration;

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{PaneEntry, Space, TabEntry, TermLine};
use crate::features::terminal::domain::repositories::TerminalRepository;

/// Simulated round-trip latency on every call — see the [module docs](self).
const MOCK_LATENCY: Duration = Duration::from_millis(20);

/// The synthetic tabs [`add_test_tab`] appends to every later `list_tabs`
/// answer — see the [module docs](self)' headless-test tab seam.
static TEST_TABS: Mutex<Vec<TabEntry>> = Mutex::new(Vec::new());

/// Append a tab keyed (and labelled) `key` to every later `list_tabs` answer, for a
/// headless test that needs the tab world to move behind the screen's back — see
/// the [module docs](self). Never called by production code.
pub fn add_test_tab(key: &str) {
    let mut tabs = lock_test_tabs();
    // Past the four seeded tabs, so an appended tab's index/id collides with
    // nothing the seed already carries.
    let index = seed_tabs().len() as u32 + tabs.len() as u32 + 1;
    tabs.push(test_tab(index, key));
}

/// Drop every tab [`add_test_tab`] appended — a suite's own teardown, and its
/// setup, since this store outlives any one mount of the screen.
pub fn clear_test_tabs() {
    lock_test_tabs().clear();
}

/// The appended-tab store, poisoned locks recovered like every other lock
/// here.
fn lock_test_tabs() -> std::sync::MutexGuard<'static, Vec<TabEntry>> {
    TEST_TABS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One appended tab, shaped exactly like a seeded one (see [`seed_tabs`]) so
/// nothing downstream can tell the two apart.
fn test_tab(index: u32, key: &str) -> TabEntry {
    TabEntry {
        index,
        tab_id: u64::from(index),
        key: key.to_string(),
        label: key.to_string(),
        pane_title: format!("ed@{key}: ~"),
        pane_name: format!("ed@{key}"),
        pane_path: "/home/ed".to_string(),
        panes: seed_panes(index, &format!("ed@{key}"), "/home/ed"),
        active: false,
    }
}

/// An in-memory [`TerminalRepository`]. See the [module docs](self).
pub struct MockTerminalRepository {
    /// The space list — one of the two things this store lets move.
    spaces: Mutex<Vec<Space>>,
    /// The tab world: a fixed set of tabs whose pane **focus** moves (see the
    /// [module docs](self)' "What mutates").
    tabs: Mutex<Vec<TabEntry>>,
}

impl MockTerminalRepository {
    /// A repository seeded with [`seed_spaces`]/[`seed_tabs`].
    pub fn seeded() -> Self {
        Self {
            spaces: Mutex::new(seed_spaces()),
            tabs: Mutex::new(seed_tabs()),
        }
    }

    /// Select the space with id `id`, deselecting every other — the body
    /// [`TerminalRepository::switch_space`] runs, exposed synchronously so a test
    /// can move the selection without a runtime.
    pub fn select_space(&self, id: &str) {
        for space in self.lock().iter_mut() {
            space.selected = space.id == id;
        }
    }

    /// The space list, cloned — a synchronous peek for tests, never part of
    /// the trait contract production code speaks.
    pub fn spaces_snapshot(&self) -> Vec<Space> {
        self.lock().clone()
    }

    /// The seeded tabs plus whatever [`add_test_tab`] appended, cloned.
    fn tabs_snapshot(&self) -> Vec<TabEntry> {
        let mut tabs = self.lock_tabs().clone();
        tabs.extend(lock_test_tabs().iter().cloned());
        tabs
    }

    /// The store, with a poisoned lock recovered rather than propagated: a
    /// panicking test must not cascade into every later call.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Space>> {
        self.spaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The tab store, recovered the same way [`lock`](Self::lock) is.
    fn lock_tabs(&self) -> std::sync::MutexGuard<'_, Vec<TabEntry>> {
        self.tabs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for MockTerminalRepository {
    fn default() -> Self {
        Self::seeded()
    }
}

/// The seeded space list — `docs/terminal.html`'s `spaces` pill row, in order,
/// with `forgekit` selected (`.pill.sel`).
fn seed_spaces() -> Vec<Space> {
    [
        ("cupline", false),
        ("zelli_mobile", false),
        ("forgekit", true),
    ]
    .into_iter()
    .map(|(name, selected)| Space {
        id: name.to_string(),
        name: name.to_string(),
        selected,
    })
    .collect()
}

/// The seeded tab world — `docs/terminal.html`'s `TABS` map, in the order its
/// `#sheetTabRow` pills draw, with `devbox` active (`.pill.sel`). Each tab also
/// carries the **pane list** the sheet lists and commits against: the mock's own
/// `.pane-item` plus one unfocused sibling, so the browse-then-commit flow has more
/// than one row to move a focus between. Pane ids are `<tab index>0`/`<tab index>1`
/// — this store fronts no backend, so an id only has to be stable and unique.
fn seed_tabs() -> Vec<TabEntry> {
    let tab = |index: u32, key: &str, label: &str, title: &str, name: &str, path: &str| TabEntry {
        index,
        tab_id: u64::from(index),
        key: key.to_string(),
        label: label.to_string(),
        pane_title: title.to_string(),
        pane_name: name.to_string(),
        pane_path: path.to_string(),
        panes: seed_panes(index, name, path),
        active: key == "devbox",
    };
    vec![
        tab(
            1,
            "btop",
            "btop",
            "ed@precision: ~",
            "ed@precision",
            "/home/ed",
        ),
        tab(
            2,
            "minefit",
            "minefit-api",
            "ed@minefit: ~/api",
            "ed@minefit",
            "~/api",
        ),
        tab(
            3,
            "gohora",
            "gohora",
            "ed@gohora: ~/src",
            "ed@gohora",
            "~/src",
        ),
        tab(
            4,
            "devbox",
            "devbox",
            "ed@devbox: ~",
            "ed@devbox",
            "/home/ed",
        ),
    ]
}

/// One tab's two panes: the mock's own focused `.pane-item`, and an unfocused `zsh` beside it
/// — see [`seed_tabs`].
///
/// Side by side, splitting a classic 80x24 grid down the middle, rather than sharing one
/// full-grid rect: `presentation::pages`' content-tap hit test resolves a tapped cell against
/// these rects, and identical ones would resolve every in-bounds tap to the same pane, leaving
/// no cell to aim at the *other* one. Two disjoint halves make both panes tappable, which is
/// what a tap's focus-or-keyboard verdict turns on.
fn seed_panes(index: u32, name: &str, path: &str) -> Vec<PaneEntry> {
    let pane = |id: u32, title: String, focused: bool, x: u32| PaneEntry {
        id,
        title,
        cwd: path.to_string(),
        focused,
        floating: false,
        exited: false,
        is_plugin: false,
        is_fullscreen: false,
        x,
        y: 0,
        rows: 24,
        cols: 40,
    };
    vec![
        pane(index * 10, name.to_string(), true, 0),
        pane(index * 10 + 1, format!("{name} zsh"), false, 40),
    ]
}

/// One tab's lines — `TABS[key].lines`, transcribed span for span (`t:'p'` →
/// [`TermLine::prompt`], `'out'` → output, `'cm'` → comment, and the trailing
/// `cursor:true` prompt → [`TermLine::caret`]).
fn seed_lines(key: &str) -> Option<Vec<TermLine>> {
    let lines = match key {
        "btop" => vec![
            TermLine::prompt("ed@precision:~$ btop"),
            TermLine::output("cpu 12% \u{b7} mem 4.1/16gb \u{b7} net 1.2mb/s"),
            TermLine::comment("# live system monitor, refreshes every 2s"),
            TermLine::caret("ed@precision:~$ "),
        ],
        "minefit" => vec![
            TermLine::prompt("ed@minefit:~/api$ cargo run --release"),
            TermLine::output("Compiling minefit-api v0.3.2"),
            TermLine::output("Finished release [optimized] in 12.4s"),
            TermLine::caret("ed@minefit:~/api$ "),
        ],
        "gohora" => vec![
            TermLine::prompt("ed@gohora:~/src$ go build ./..."),
            TermLine::output("ok  gohora/cmd/server  0.842s"),
            TermLine::comment("# no errors, 3 packages rebuilt"),
            TermLine::caret("ed@gohora:~/src$ "),
        ],
        "devbox" => vec![
            TermLine::prompt("ed@devbox:~$ systemctl status muxrd"),
            TermLine::output("Active: active (running) since Jul 04 10:31:23"),
            TermLine::comment("# staggered per-line reveal \u{2014} replays on tab switch"),
            TermLine::caret("ed@devbox:~$ "),
        ],
        _ => return None,
    };
    Some(lines)
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl TerminalRepository for MockTerminalRepository {
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        Ok(self.spaces_snapshot())
    }

    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        Ok(self.tabs_snapshot())
    }

    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        seed_lines(&key).ok_or_else(|| AppFailure::Internal(format!("tab_lines: no tab {key}")))
    }

    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        if !self.lock().iter().any(|space| space.id == space_id) {
            return Err(AppFailure::Internal(format!(
                "switch_space: no space {space_id}"
            )));
        }
        self.select_space(&space_id);
        Ok(())
    }

    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        // Nothing to tell: this store fronts no session, and the card's own switch
        // is `tab_lines`. An id no seeded tab carries is still a bug worth
        // surfacing — the sheet only addresses a tab it just drew.
        if !self.tabs_snapshot().iter().any(|tab| tab.tab_id == tab_id) {
            return Err(AppFailure::Internal(format!("go_to_tab: no tab {tab_id}")));
        }
        Ok(())
    }

    async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        if id.trim().is_empty() {
            return Err(AppFailure::Internal(
                "detach_session called with a blank id".to_string(),
            ));
        }
        // Nothing to drop: there is no real attachment until the `muxrd`
        // client lands (see the module docs).
        Ok(())
    }

    async fn focus_pane(&self, pane_id: u32, is_plugin: bool) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        // A pane no seeded tab carries is a bug worth surfacing, since the
        // sheet only ever commits a row it just drew.
        let addressed = |pane: &PaneEntry| pane.id == pane_id && pane.is_plugin == is_plugin;
        let mut tabs = self.lock_tabs();
        let Some(tab) = tabs.iter_mut().find(|tab| tab.panes.iter().any(addressed)) else {
            return Err(AppFailure::Internal(format!(
                "focus_pane: no pane {pane_id}"
            )));
        };
        // One focused pane per tab, and it is the one just addressed — so a client
        // re-reading the layout is told what it just asked for.
        for pane in &mut tab.panes {
            let focused = addressed(pane);
            pane.focused = focused;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::terminal::domain::entities::TermLineKind;

    /// Drive one repository call to completion on a current-thread runtime —
    /// a pure data-layer test, with no frust runtime in play.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn the_seeded_spaces_are_the_mocks_pill_row() {
        let repo = MockTerminalRepository::seeded();
        let spaces = block_on(repo.list_spaces()).expect("the mock never fails to list");

        let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["cupline", "zelli_mobile", "forgekit"]);
        let selected: Vec<&str> = spaces
            .iter()
            .filter(|s| s.selected)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(selected, ["forgekit"], "exactly one `.pill.sel`");
    }

    #[test]
    fn the_seeded_tabs_are_the_mocks_four_with_devbox_active() {
        let repo = MockTerminalRepository::seeded();
        let tabs = block_on(repo.list_tabs()).expect("the mock never fails to list");

        let pills: Vec<String> = tabs.iter().map(TabEntry::pill_label).collect();
        assert_eq!(pills, ["1 btop", "2 minefit-api", "3 gohora", "4 devbox"]);
        let active: Vec<&str> = tabs
            .iter()
            .filter(|t| t.active)
            .map(|t| t.key.as_str())
            .collect();
        assert_eq!(active, ["devbox"]);

        // The pane facts the card's status row and the sheet's pane item print.
        let devbox = tabs.last().expect("four tabs");
        assert_eq!(devbox.pane_title, "ed@devbox: ~");
        assert_eq!(devbox.pane_name, "ed@devbox");
        assert_eq!(devbox.pane_path, "/home/ed");
    }

    #[test]
    fn the_active_tabs_lines_are_the_mocks_verbatim() {
        let repo = MockTerminalRepository::seeded();
        let lines = block_on(repo.tab_lines("devbox".to_string())).expect("devbox exists");

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].kind, TermLineKind::Prompt);
        assert_eq!(lines[0].text, "ed@devbox:~$ systemctl status muxrd");
        assert_eq!(lines[1].kind, TermLineKind::Output);
        assert_eq!(
            lines[1].text,
            "Active: active (running) since Jul 04 10:31:23"
        );
        assert_eq!(lines[2].kind, TermLineKind::Comment);
        assert_eq!(
            lines[2].text,
            "# staggered per-line reveal \u{2014} replays on tab switch"
        );
        assert_eq!(lines[3].text, "ed@devbox:~$ ");
        assert!(
            lines[3].cursor,
            "only the trailing prompt carries the caret"
        );
        assert_eq!(
            lines.iter().filter(|l| l.cursor).count(),
            1,
            "one caret per tab"
        );
    }

    #[test]
    fn every_tab_has_lines_and_an_unknown_key_fails() {
        let repo = MockTerminalRepository::seeded();
        for key in ["btop", "minefit", "gohora", "devbox"] {
            let lines = block_on(repo.tab_lines(key.to_string()))
                .unwrap_or_else(|_| panic!("{key} has lines"));
            assert_eq!(lines.len(), 4, "{key}");
            assert!(
                lines.last().is_some_and(|l| l.cursor),
                "{key} ends in a caret"
            );
        }
        assert!(block_on(repo.tab_lines("nope".to_string())).is_err());
    }

    #[test]
    fn selecting_a_space_moves_the_selection_rather_than_adding_one() {
        let repo = MockTerminalRepository::seeded();
        repo.select_space("cupline");

        let selected: Vec<String> = repo
            .spaces_snapshot()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.name)
            .collect();
        assert_eq!(selected, ["cupline"]);
    }

    #[test]
    fn a_space_switch_writes_the_store_and_an_unknown_id_fails() {
        let repo = MockTerminalRepository::seeded();
        assert!(block_on(repo.switch_space("cupline".to_string())).is_ok());

        let selected: Vec<String> = repo
            .spaces_snapshot()
            .into_iter()
            .filter(|s| s.selected)
            .map(|s| s.id)
            .collect();
        assert_eq!(selected, ["cupline"], "the switch is the one write path");
        assert!(block_on(repo.switch_space("nope".to_string())).is_err());
    }

    #[test]
    fn a_tab_switch_succeeds_for_every_seeded_id_and_fails_for_no_other() {
        let repo = MockTerminalRepository::seeded();
        for tab_id in 1..=4 {
            assert!(block_on(repo.go_to_tab(tab_id)).is_ok(), "{tab_id}");
        }
        assert!(block_on(repo.go_to_tab(99)).is_err());
    }

    #[test]
    fn every_tab_carries_two_panes_with_exactly_one_focused() {
        let repo = MockTerminalRepository::seeded();
        let tabs = block_on(repo.list_tabs()).expect("the mock never fails to list");

        for tab in &tabs {
            assert_eq!(tab.panes.len(), 2, "{}", tab.key);
            let focused: Vec<u32> = tab
                .panes
                .iter()
                .filter(|pane| pane.focused)
                .map(|pane| pane.id)
                .collect();
            assert_eq!(focused, [tab.index * 10], "{}", tab.key);
            // The mock's own `.pane-item` is the focused one, verbatim.
            assert_eq!(tab.panes[0].title, tab.pane_name);
            assert_eq!(tab.panes[0].cwd, tab.pane_path);
        }
        // …and no two panes share an id across the whole seed.
        let mut ids: Vec<u32> = tabs
            .iter()
            .flat_map(|t| t.panes.iter().map(|p| p.id))
            .collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "pane ids are unique");
    }

    #[test]
    fn a_pane_focus_succeeds_for_every_seeded_pane_and_fails_for_no_other() {
        let repo = MockTerminalRepository::seeded();
        for tab in block_on(repo.list_tabs()).expect("the mock never fails to list") {
            for pane in tab.panes {
                assert!(
                    block_on(repo.focus_pane(pane.id, pane.is_plugin)).is_ok(),
                    "{}",
                    pane.id
                );
            }
        }
        assert!(block_on(repo.focus_pane(999, false)).is_err());
        // The two id namespaces are separate — a plugin pane with a terminal
        // pane's id is still not a pane this store carries.
        assert!(block_on(repo.focus_pane(10, true)).is_err());
    }

    #[test]
    fn a_pane_focus_moves_the_focused_flag_so_a_later_list_agrees_with_it() {
        let repo = MockTerminalRepository::seeded();
        // The second pane of the first tab — the mock seeds the first focused.
        assert!(block_on(repo.focus_pane(11, false)).is_ok());

        let tabs = block_on(repo.list_tabs()).expect("the mock never fails to list");
        let focused: Vec<u32> = tabs[0]
            .panes
            .iter()
            .filter(|pane| pane.focused)
            .map(|pane| pane.id)
            .collect();
        assert_eq!(
            focused,
            [11],
            "one focused pane per tab, and it is the one just addressed"
        );
        assert_eq!(
            tabs[1]
                .panes
                .iter()
                .filter(|pane| pane.focused)
                .map(|pane| pane.id)
                .collect::<Vec<u32>>(),
            [20],
            "…and no other tab's focus moved with it"
        );
    }

    #[test]
    fn an_appended_test_tab_is_shaped_exactly_like_a_seeded_one() {
        // The seam's *effect* on `list_tabs` is exercised from
        // `tests/terminal_live.rs`, whose suite is serialized — this binary runs its
        // tests in parallel, and the appended-tab store is process-global.
        let appended = test_tab(5, "scratch");
        assert_eq!(appended.pill_label(), "5 scratch");
        assert_eq!(appended.tab_id, 5);
        assert!(
            !seed_tabs().iter().any(|tab| tab.tab_id == appended.tab_id),
            "an appended tab's id collides with no seeded one"
        );
        assert_eq!(
            appended.panes.len(),
            seed_tabs()[0].panes.len(),
            "…and it carries the same pane shape"
        );
    }

    #[test]
    fn detach_rejects_a_blank_id() {
        let repo = MockTerminalRepository::seeded();
        assert!(block_on(repo.detach_session("s-1".to_string())).is_ok());
        assert!(block_on(repo.detach_session("  ".to_string())).is_err());
    }
}
