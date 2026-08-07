//! [`SessionsController`] — the `/sessions` screen's view model.
//!
//! Mirrors `examples/huddle`'s controller shape (and this app's own
//! [`crate::connection::ConnectionController`]): a [`ControllerCore`] embedded
//! by composition, the feature's four use cases constructed over one injected
//! repository, and the signals the screen renders. Hosted by the screen through
//! `clean_signals_frust::use_controller`, so it is created on route mount and
//! disposed on unmount.
//!
//! # Signals vs. plain state
//!
//! [`sessions`](Self::sessions) and [`loaded`](Self::loaded) are `RwSignal`s —
//! they are observed state, and writing one is what wakes the screen's rebuild.
//! The poll gate below is **not** a signal on purpose: it is written from
//! `Component::build` (see [`observe_depth`](Self::observe_depth)), and a
//! tracked write there would re-dirty the very frame that made it.
//!
//! # The mock poll, and its page-driven pause
//!
//! The Flutter app polls the daemon for layout changes while the sessions page
//! is showing and calls `stopPolling()` before pushing the terminal route. The
//! frust mapping is *not* automatic: `NavigatorWidget` keeps a covered page's
//! retained widgets **and re-runs its builder every rebuild** (its "Paint
//! culling" docs; `rebuild` step 2 reconciles every retained page, including
//! culled ones), so a screen under `/terminal` is still very much alive — its
//! component is not disposed, and an interval hosted by it keeps firing. The
//! pause therefore has to be explicit, and it is:
//!
//! - [`pause_until_depth`](Self::pause_until_depth) is called by the screen
//!   immediately before every push it makes (the terminal route, a dialog, the
//!   picker sheet), carrying the navigator depth *before* the push;
//! - [`observe_depth`](Self::observe_depth) is called from the screen's `build`
//!   with the live depth, and resumes the moment the stack is back at (or
//!   below) that mark.
//!
//! That pair is depth-arithmetic-free at mount time — deliberately, because at
//! `Component::init` the navigator has not yet pushed this page onto its own
//! stack (`apply_ops` builds the page *before* `pages.push`), so a
//! mount-time baseline would be one short.
//!
//! # Poll cadence, loaded once per mount
//!
//! The mock poll's *period* used to be the hardcoded [`POLL_PERIOD_SECS`]
//! constant; it now comes from `settings`' own stored `layout_refresh_secs`
//! preference, resolved by an async load `pages.rs`'s `Component::init`
//! kicks off (`settings::di::expect_preferences_repository()`, the same
//! di.rs-is-composition-root-wiring widening `terminal`'s page already uses
//! for the identical repository — `docs/ARCHITECTURE.md`'s Feature Slices
//! section). [`poll_period`](Self::poll_period) holds the result: `None`
//! until that load resolves, `Some(secs)` once it does (falling back to
//! [`POLL_PERIOD_SECS`] on a load failure, which never actually happens in
//! practice — both the mock and the real preferences repository resolve
//! every field to a default rather than erroring). `Component::build` starts
//! `clean_signals_frust::use_interval` the moment this flips to `Some`,
//! **exactly once** per mount ([`claim_poll_interval_start`](Self::claim_poll_interval_start)
//! guards the once-ness, since `build` re-runs on every tracked write and
//! `poll_period` stays `Some` forever after) — see `pages.rs`'s own docs for
//! why the call has to live in `build` rather than `init`. A pref changed
//! while this screen is *already* mounted therefore does not retroactively
//! reschedule this mount's interval — it applies from the **next** Sessions
//! mount, which re-runs the whole load-then-start sequence fresh.
//!
//! `use_interval` itself, per its own doc, must be called where a reactive
//! `Owner` is ambient; `ComponentWidget::rebuild` (`frust-core`'s own
//! `component` module) runs `Component::build` under the *same* owner
//! `Component::init` does, so starting it from `build` is exactly as sound
//! as starting it from `init` — it just gets to wait for a value `init`
//! couldn't have had synchronously.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use clean_signals::{ControllerCore, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{RenameRequest, Session};
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::features::sessions::domain::use_cases::{
    CreateSession, KillSession, ListSessions, RenameSession,
};

/// The sentinel [`SessionsController::resume_at`] holds while polling is
/// running — no real navigator depth ever reaches it.
const NOT_PAUSED: usize = usize::MAX;

/// The mock poll's fallback period, in seconds, used only if the async load
/// of `settings`' own `layout_refresh_secs` preference fails (see the
/// [module docs](self)' "Poll cadence" section) — matches
/// `docs/redesign.html`'s settings row ("layout refresh — every 3s"), the
/// same value `settings::domain::entities::Preferences::defaults` ships.
pub const POLL_PERIOD_SECS: u32 = 3;

/// The `/sessions` screen's view model. See the [module docs](self).
pub struct SessionsController {
    core: ControllerCore<AppFailure>,
    list: ListSessions,
    create: CreateSession,
    rename: RenameSession,
    kill: KillSession,
    /// The session set as last read from the repository — the list the screen
    /// renders.
    pub sessions: RwSignal<Vec<Session>>,
    /// Whether a first load has completed. The empty state renders only once
    /// this is `true`, so a not-yet-loaded screen shows an empty list rather
    /// than "no sessions".
    pub loaded: RwSignal<bool>,
    /// The navigator depth to resume polling at, or [`NOT_PAUSED`]. See the
    /// [module docs](self)'s pause note.
    resume_at: AtomicUsize,
    /// Mock poll ticks actually taken (i.e. not skipped by the pause) — the
    /// observable the pause's own tests assert on.
    polls: AtomicU32,
    /// The poll's resolved cadence — `None` until the async preferences load
    /// [`pages.rs`](super::pages) kicks off from `Component::init` resolves.
    /// See the [module docs](self)' "Poll cadence" section.
    pub poll_period: RwSignal<Option<u32>>,
    /// Guards [`claim_poll_interval_start`](Self::claim_poll_interval_start)'s
    /// once-only contract.
    poll_interval_started: AtomicBool,
    /// Whether a poll-triggered refresh is currently outstanding — the
    /// in-flight claim that bounds the mock poll's own concurrency to one.
    /// Without it, an RPC round trip slower than the poll's own period lets
    /// every later tick stack another full fan-out on top of the ones still
    /// running, growing network traffic without bound.
    /// [`take_poll_tick`](Self::take_poll_tick) claims it (an `AtomicBool`
    /// swap, the same single-flight shape
    /// `ConnectController::begin_credential_save`'s own `saving_credential`
    /// uses) before letting a tick through; [`refresh_from_poll`](Self::refresh_from_poll)
    /// releases it once that tick's own refresh finishes. Deliberately scoped
    /// to the poll alone — a mutation's own trailing refresh
    /// (create/rename/kill) calls [`refresh`](Self::refresh) directly and is
    /// never gated by it, so a user action is never silently dropped just
    /// because a poll happens to be mid-flight; [`generation`](Self::generation)
    /// is what keeps *that* case honest instead.
    poll_in_flight: AtomicBool,
    /// Bumped at the start of every [`refresh`](Self::refresh) call — the
    /// staleness guard [`refresh`](Self::refresh) checks before writing
    /// [`sessions`](Self::sessions): [`sessions`](Self::sessions) is
    /// last-writer-wins, so without this an older, slower refresh that lands
    /// *after* a newer one (typically the poll's, racing a mutation's own
    /// trailing refresh) would clobber the fresher result. A refresh
    /// snapshots the generation it bumped to before its network round trip,
    /// and only writes if it is still the newest one by the time that round
    /// trip lands.
    generation: AtomicU64,
}

impl SessionsController {
    /// A fresh controller over the injected repository (the composition root
    /// built it once — see [`crate::features::sessions::di`]).
    pub fn new(repository: Arc<dyn SessionsRepository + Send + Sync>) -> Self {
        Self {
            core: ControllerCore::new(),
            list: ListSessions::new(Arc::clone(&repository)),
            create: CreateSession::new(Arc::clone(&repository)),
            rename: RenameSession::new(Arc::clone(&repository)),
            kill: KillSession::new(repository),
            sessions: RwSignal::new(Vec::new()),
            loaded: RwSignal::new(false),
            resume_at: AtomicUsize::new(NOT_PAUSED),
            polls: AtomicU32::new(0),
            poll_period: RwSignal::new(None),
            poll_interval_started: AtomicBool::new(false),
            poll_in_flight: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }

    /// This controller's [`ControllerCore`] — the failure sink a screen
    /// subscribes its toast bridge to (`use_failure_listener`). A named
    /// accessor rather than the [`AsRef`] impl below because
    /// `Arc<SessionsController>` has its own inherent `as_ref`, which would
    /// make the bare call ambiguous at every site.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// Load (or reload) the session list from the repository. Every mutation
    /// below ends with one of these, so the screen always renders what the
    /// server actually holds rather than a locally patched guess.
    ///
    /// Every call — the first load, a mutation's trailing refresh, or a poll
    /// tick's [`refresh_from_poll`](Self::refresh_from_poll) — bumps
    /// [`generation`](Self::generation) up front and only writes
    /// [`sessions`](Self::sessions)/[`loaded`](Self::loaded) if that snapshot
    /// is still the newest one once the round trip lands, so a call that
    /// started earlier but resolves later can never clobber a fresher result
    /// that already landed.
    pub async fn refresh(&self) {
        let my_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(sessions) = self
            .core
            .run(&self.list, NoParams, RunOptions::default())
            .await
            && self.generation.load(Ordering::SeqCst) == my_generation
        {
            self.sessions.set(sessions);
            self.loaded.set(true);
        }
    }

    /// The refresh a claimed poll tick runs — identical to
    /// [`refresh`](Self::refresh), wrapped with the release of the in-flight
    /// claim [`take_poll_tick`](Self::take_poll_tick) took for it. The
    /// release is an RAII guard rather than a trailing statement so a future
    /// early return added to either function can't leak the claim and wedge
    /// the poll shut.
    pub async fn refresh_from_poll(&self) {
        struct ReleasePollClaim<'a>(&'a AtomicBool);
        impl Drop for ReleasePollClaim<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _release = ReleasePollClaim(&self.poll_in_flight);
        self.refresh().await;
    }

    /// Create a session named `name` (already trimmed by the dialog), then
    /// refresh. A failed create leaves the list untouched and surfaces through
    /// [`failures`](ControllerCore::failures) — the screen's toast bridge.
    pub async fn create(&self, name: String) {
        if self
            .core
            .run(&self.create, name, RunOptions::default())
            .await
            .is_ok()
        {
            self.refresh().await;
        }
    }

    /// Rename the session with `id`, then refresh.
    pub async fn rename(&self, id: String, name: String) {
        if self
            .core
            .run(
                &self.rename,
                RenameRequest { id, name },
                RunOptions::default(),
            )
            .await
            .is_ok()
        {
            self.refresh().await;
        }
    }

    /// Kill the session with `id`, then refresh.
    pub async fn kill(&self, id: String) {
        if self
            .core
            .run(&self.kill, id, RunOptions::default())
            .await
            .is_ok()
        {
            self.refresh().await;
        }
    }

    /// The session with `id` in the last loaded set, if it is still there —
    /// how an action closure (which captured only an id) recovers the current
    /// name to seed the rename dialog with.
    pub fn session(&self, id: &str) -> Option<Session> {
        self.sessions
            .get_untracked()
            .into_iter()
            .find(|s| s.id == id)
    }

    // -- the mock poll -----------------------------------------------------

    /// **Consume** one mock poll tick, answering whether the caller should
    /// refresh now. A paused controller skips, and so does one whose first
    /// load hasn't landed yet (the screen's own initial load is already in
    /// flight; a second concurrent list would race it for no benefit) —
    /// and so does one whose *previous* tick is still running, claimed via
    /// [`poll_in_flight`](Self::poll_in_flight): the `&&` short-circuits
    /// before the claim swap unless both other gates already pass, so a
    /// paused or not-yet-loaded controller never touches the flag at all.
    /// Only a taken tick counts towards [`polls_taken`](Self::polls_taken).
    ///
    /// The caller drives the timing (`clean_signals_frust::use_interval` in
    /// the screen's `build`, once [`poll_period`](Self::poll_period)
    /// resolves — see the [module docs](self)' "Poll cadence" section) and
    /// the async spawn (through [`refresh_from_poll`](Self::refresh_from_poll),
    /// which releases the claim this takes), so this stays synchronous and
    /// unit-testable.
    pub fn take_poll_tick(&self) -> bool {
        let poll = self.is_polling()
            && self.loaded.get_untracked()
            && !self.poll_in_flight.swap(true, Ordering::SeqCst);
        if poll {
            self.polls.fetch_add(1, Ordering::Relaxed);
        }
        poll
    }

    /// Claims the one-time right to start the mock poll's `use_interval`.
    /// Returns `true` exactly once per controller (the caller then starts
    /// the interval), `false` on every later call — `Component::build`
    /// re-runs on every tracked write, and [`poll_period`](Self::poll_period)
    /// stays `Some` forever after it first resolves, so without this a
    /// second rebuild would start a second interval.
    pub fn claim_poll_interval_start(&self) -> bool {
        self.poll_interval_started
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    /// Whether the mock poll is currently running (i.e. nothing this screen
    /// pushed is still on top of it).
    pub fn is_polling(&self) -> bool {
        self.resume_at.load(Ordering::Relaxed) == NOT_PAUSED
    }

    /// Poll ticks actually taken so far — observability for the pause's tests,
    /// never read by the UI.
    pub fn polls_taken(&self) -> u32 {
        self.polls.load(Ordering::Relaxed)
    }

    /// Pause the poll until the navigator stack is back at `depth` — called by
    /// the screen with `nav.depth()` *before* it pushes anything. Repeated
    /// calls keep the shallowest mark, so a dialog opened over an already
    /// paused screen doesn't shorten the pause.
    pub fn pause_until_depth(&self, depth: usize) {
        self.resume_at.fetch_min(depth, Ordering::Relaxed);
    }

    /// Observe the live navigator depth (called from the screen's `build`) and
    /// resume the poll once the stack is back at — or below — the mark
    /// [`pause_until_depth`](Self::pause_until_depth) set.
    pub fn observe_depth(&self, depth: usize) {
        let mark = self.resume_at.load(Ordering::Relaxed);
        if mark != NOT_PAUSED && depth <= mark {
            self.resume_at.store(NOT_PAUSED, Ordering::Relaxed);
        }
    }
}

impl AsRef<ControllerCore<AppFailure>> for SessionsController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::sessions::data::repositories::MockSessionsRepository;

    /// How many sessions the mock repository seeds (the redesign's three
    /// cards) — named here so a reseed moves one constant, not six literals.
    const SEEDED_COUNT: usize = 3;

    /// The test-wiring composition root: the mock repository behind the domain
    /// trait object. `#[cfg(test)]`-only wiring, so `di.rs` stays the only
    /// production site where presentation meets `data` (huddle's convention).
    fn controller() -> SessionsController {
        SessionsController::new(Arc::new(MockSessionsRepository::seeded()))
    }

    /// Drive a controller call to completion on a current-thread runtime.
    /// These tests exercise the controller's own logic, with no frust
    /// executor/owner in play (`RwSignal`s work fine outside one).
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn refresh_loads_the_seeded_sessions_and_flips_loaded() {
        let c = controller();
        assert!(!c.loaded.get_untracked(), "nothing is loaded up front");

        block_on(c.refresh());

        assert_eq!(c.sessions.get_untracked().len(), SEEDED_COUNT);
        assert!(c.loaded.get_untracked());
    }

    #[test]
    fn create_round_trips_through_the_repository() {
        let c = controller();
        block_on(c.refresh());

        block_on(c.create("staging".to_string()));

        let names: Vec<String> = c
            .sessions
            .get_untracked()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names.len(), SEEDED_COUNT + 1);
        assert!(names.contains(&"staging".to_string()), "{names:?}");
    }

    #[test]
    fn rename_round_trips_and_keeps_the_list_length() {
        let c = controller();
        block_on(c.refresh());
        let id = c.sessions.get_untracked()[0].id.clone();

        block_on(c.rename(id.clone(), "dev-2".to_string()));

        assert_eq!(c.sessions.get_untracked().len(), SEEDED_COUNT);
        assert_eq!(c.session(&id).map(|s| s.name), Some("dev-2".to_string()));
    }

    #[test]
    fn kill_removes_the_session_and_emptying_the_list_is_reachable() {
        let c = controller();
        block_on(c.refresh());

        for session in c.sessions.get_untracked() {
            block_on(c.kill(session.id));
        }

        assert!(c.sessions.get_untracked().is_empty());
        assert!(c.loaded.get_untracked(), "…and it is a *loaded* empty list");
    }

    #[test]
    fn a_failing_call_leaves_the_list_alone() {
        let c = controller();
        block_on(c.refresh());
        let before = c.sessions.get_untracked();

        // No such session — the mock maps an unknown id to `Internal`.
        block_on(c.kill("nope".to_string()));

        assert_eq!(c.sessions.get_untracked(), before);
    }

    #[test]
    fn the_poll_runs_only_once_a_first_load_has_landed() {
        let c = controller();
        assert!(!c.take_poll_tick(), "no poll before the first load");

        block_on(c.refresh());

        assert!(c.take_poll_tick());
        assert_eq!(c.polls_taken(), 1);
    }

    #[test]
    fn a_push_pauses_the_poll_until_the_stack_comes_back() {
        let c = controller();
        block_on(c.refresh());
        assert!(c.is_polling());

        // The screen sits at depth 1 and pushes something over itself.
        c.pause_until_depth(1);
        assert!(!c.is_polling(), "the poll pauses for the pushed page");
        assert!(!c.take_poll_tick());
        assert_eq!(c.polls_taken(), 0, "a skipped tick is not counted");

        // Still covered: a build at depth 2 must not resume it.
        c.observe_depth(2);
        assert!(!c.is_polling());

        // Popped back: the next build resumes it.
        c.observe_depth(1);
        assert!(c.is_polling());
        assert!(c.take_poll_tick());
    }

    #[test]
    fn nested_pushes_keep_the_shallowest_resume_mark() {
        let c = controller();
        block_on(c.refresh());

        c.pause_until_depth(1);
        // A dialog opened *over* the pushed page must not shorten the pause.
        c.pause_until_depth(2);
        c.observe_depth(2);
        assert!(!c.is_polling(), "still covered by the first push");

        c.observe_depth(1);
        assert!(c.is_polling());
    }

    #[test]
    fn poll_period_starts_unresolved() {
        let c = controller();
        assert_eq!(c.poll_period.get_untracked(), None);
    }

    #[test]
    fn claim_poll_interval_start_succeeds_exactly_once() {
        let c = controller();
        assert!(c.claim_poll_interval_start(), "the first claim wins");
        assert!(
            !c.claim_poll_interval_start(),
            "a second claim (a later rebuild) must not win again"
        );
        assert!(!c.claim_poll_interval_start());
    }
}
