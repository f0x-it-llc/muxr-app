//! [`MockTerminalRepository`] — the in-memory [`TerminalRepository`] the mock
//! era runs on.
//!
//! Seeds **exactly** what `docs/terminal.html` (v2) ships: the sheet's three
//! spaces (`cupline`, `zelli_mobile`, `forgekit` selected), its four tabs
//! (`1 btop`, `2 minefit-api`, `3 gohora`, `4 devbox` active), and each tab's
//! title/name/path plus its four `.term-line`s — every string transcribed from
//! that file's own `TABS` map and sheet markup rather than paraphrased, so the
//! screen and the mock are the same picture. `tests/terminal.rs` asserts the
//! seed against those strings, which is what keeps a later edit to one from
//! silently diverging from the other.
//!
//! # Framework-free, and deliberately not `frust`-aware
//!
//! No `frust::` types here (the data-layer rule): the store is a plain
//! [`Mutex`]-guarded space list plus immutable tab data. The mutex — not a
//! `RefCell` — is what makes this `Send + Sync`, which
//! [`TerminalController`](crate::features::terminal::presentation::controller::TerminalController)
//! needs to hold it behind an `Arc` under `use_controller`'s bounds.
//!
//! # What mutates, and what does not
//!
//! Space selection is real state (the sheet's space pills toggle it), so it
//! lives behind the mutex. Tabs and lines are **fixed**: v2's mock replays the
//! same four tabs regardless of which session was attached (see
//! `domain::entities`' module docs), and a tab switch is a *read*, not a
//! write. `detach_session` is a no-op that merely validates its id — there is
//! no attachment to drop until the real `muxrd` client lands.
//!
//! # Failure mapping
//!
//! This is the feature's single failure-mapping site into [`AppFailure`]: an
//! unknown tab key or a blank session id maps to [`AppFailure::Internal`] (a
//! bug — the screen only ever addresses what it just rendered). A real gRPC
//! client maps transport errors to [`AppFailure::Network`] and a rejected
//! token to [`AppFailure::Auth`] in exactly this spot.
//!
//! # Latency
//!
//! [`MOCK_LATENCY`] is a short, deliberate `sleep` before every call so the
//! screen's async path (spawn → await → signal write → rebuild) is the one the
//! real client will take — the same device (and the same value) the sessions
//! feature's own mock repository uses.

use std::sync::Mutex;
use std::time::Duration;

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::{Space, TabEntry, TermLine};
use crate::features::terminal::domain::repositories::TerminalRepository;

/// Simulated round-trip latency on every call — see the [module docs](self).
const MOCK_LATENCY: Duration = Duration::from_millis(20);

/// An in-memory [`TerminalRepository`]. See the [module docs](self).
pub struct MockTerminalRepository {
    /// The space list — the one thing on this screen that mutates.
    spaces: Mutex<Vec<Space>>,
    /// The tab world, fixed for the mock era.
    tabs: Vec<TabEntry>,
}

impl MockTerminalRepository {
    /// A repository seeded with [`seed_spaces`]/[`seed_tabs`].
    pub fn seeded() -> Self {
        Self {
            spaces: Mutex::new(seed_spaces()),
            tabs: seed_tabs(),
        }
    }

    /// Select the space named `name`, deselecting every other — the sheet's
    /// space pills. A **synchronous** mutation, deliberately outside the
    /// [`TerminalRepository`] contract: v2's spaces are selection state the
    /// sheet owns, not something the daemon is told about yet.
    pub fn select_space(&self, name: &str) {
        for space in self.lock().iter_mut() {
            space.selected = space.name == name;
        }
    }

    /// The space list, cloned — a synchronous peek for tests, never part of
    /// the trait contract production code speaks.
    pub fn spaces_snapshot(&self) -> Vec<Space> {
        self.lock().clone()
    }

    /// The store, with a poisoned lock recovered rather than propagated: a
    /// panicking test must not cascade into every later call (the sessions
    /// mock's own recovery).
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Space>> {
        self.spaces
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
        name: name.to_string(),
        selected,
    })
    .collect()
}

/// The seeded tab world — `docs/terminal.html`'s `TABS` map, in the order its
/// `#sheetTabRow` pills draw, with `devbox` active (`.pill.sel`).
fn seed_tabs() -> Vec<TabEntry> {
    vec![
        TabEntry {
            index: 1,
            key: "btop".to_string(),
            label: "btop".to_string(),
            pane_title: "ed@precision: ~".to_string(),
            pane_name: "ed@precision".to_string(),
            pane_path: "/home/ed".to_string(),
            active: false,
        },
        TabEntry {
            index: 2,
            key: "minefit".to_string(),
            label: "minefit-api".to_string(),
            pane_title: "ed@minefit: ~/api".to_string(),
            pane_name: "ed@minefit".to_string(),
            pane_path: "~/api".to_string(),
            active: false,
        },
        TabEntry {
            index: 3,
            key: "gohora".to_string(),
            label: "gohora".to_string(),
            pane_title: "ed@gohora: ~/src".to_string(),
            pane_name: "ed@gohora".to_string(),
            pane_path: "~/src".to_string(),
            active: false,
        },
        TabEntry {
            index: 4,
            key: "devbox".to_string(),
            label: "devbox".to_string(),
            pane_title: "ed@devbox: ~".to_string(),
            pane_name: "ed@devbox".to_string(),
            pane_path: "/home/ed".to_string(),
            active: true,
        },
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
        Ok(self.tabs.clone())
    }

    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        seed_lines(&key).ok_or_else(|| AppFailure::Internal(format!("tab_lines: no tab {key}")))
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
    fn detach_rejects_a_blank_id() {
        let repo = MockTerminalRepository::seeded();
        assert!(block_on(repo.detach_session("s-1".to_string())).is_ok());
        assert!(block_on(repo.detach_session("  ".to_string())).is_err());
    }
}
