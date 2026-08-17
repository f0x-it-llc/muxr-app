//! A modal bottom sheet: [`show_sheet`] pushes one over the root overlay host.
//!
//! **The chrome is `frust_glyph`'s.** The panel, its scrim, the drag handle,
//! the enter/exit staging and every dismissal gesture (scrim tap, handle
//! drag, `Escape`, Android back) belong to [`frust_glyph::glyph_sheet`] —
//! this module is the thin app-facing adapter over
//! [`show_glyph_sheet`](frust_glyph::show_glyph_sheet). What stays here is
//! genuinely the app's: the panel gutter and bottom safe-area inset a muxr
//! body is written against ([`sheet_content`], since Glyph lays its content
//! slot out under **loose** width while every muxr body expects a
//! full-content-width column), and the dismissal gate below.
//!
//! **One sheet, one dismissal — [`SheetHandle`].** A sheet stays interactive
//! until its pop actually *lands*, a rebuild after it is enqueued — so every
//! app-driven dismissal path (a second row tap, a close button pressed while
//! a delayed close is pending, a timer waking after the sheet is already
//! gone) races every other one, and calling [`NavigatorController::pop`]
//! directly from more than one of them pops **the page under the sheet**. So
//! a sheet closes only through [`SheetHandle`] — the identity [`show_sheet`]
//! mints per instance — whose [`close`](SheetHandle::close)/
//! [`close_then`](SheetHandle::close_then) close **at most once per
//! instance** and no-op after that (a duplicate close is a routine race, not
//! a bug); [`show_sheet`]'s completion callback latches the same handle so a
//! close still in flight when a user-driven dismissal wins instead cannot
//! pop anything, and [`SheetHandle::close_after`] is the delayed close with
//! latest-wins re-timing on the same gate.
//!
//! **`close` plays the same staged exit as a user dismissal**
//! ([`frust_glyph::GlyphSheetHandle::close`]) and the page pops only once
//! that exit completes, never immediately — so a close that also needs to
//! push a follow-on page cannot just push right after calling `close` (that
//! push would land ahead of the sheet's own delayed pop, and the eventual
//! pop would take the wrong page). [`close_then`](SheetHandle::close_then)
//! rides frust's composed `on_close` (pop enqueued first, then the chained
//! callback, both exactly once, on the exit's completion frame) to queue a
//! follow-on push strictly behind the sheet's own pop; a close-then-*persist*
//! site (no navigator touched) stays on plain [`close`](SheetHandle::close).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use frust::{
    AnyView, Column, CrossAxisAlignment, EdgeInsets, NavigatorController, Padding, PopResult, any,
    safe_area,
};
use frust_glyph::{GlyphSheetHandle, glyph_sheet, show_glyph_sheet};

/// Scrim opacity behind a modal, transcribed from the Glyph catalog's own
/// barrier constant (private to `frust_glyph`'s dialog and sheet, which pick
/// the same value so the two surfaces dim the app identically).
///
/// `pub` because it is the number the modal suites assert the painted scrim
/// against (`tests/ui_substrate.rs`, `tests/navigation_motion.rs`): "the sheet
/// is fully dimmed" has to be spelled once, here, rather than transcribed into
/// each test that would then pin its own copy.
pub const SCRIM_ALPHA: f32 = 0.6;

/// Panel padding on the left/right/bottom edges, in logical px — Glyph's roomy
/// `--space-5` gutter (`glyph::dialog`'s `DIALOG_PADDING`). There is no top
/// gutter: the sheet's own drag-handle strip is the band above the body.
const PANEL_PAD: f64 = 20.0;

/// Push `build_body`'s content as a modal bottom sheet over the current page.
///
/// One call is the whole interaction: the panel slides up behind a fading
/// scrim, and dismisses on a scrim tap, a drag down from the handle, `Escape`,
/// or an Android back press — all four the Glyph sheet's own, each playing the
/// staged exit before the page pops. `build_body` is re-invoked on every
/// rebuild while the sheet is up (the navigator's page-builder contract), so it
/// may read signals freely.
///
/// # The navigator it is pushed through
///
/// `nav` is what the page is pushed through (a sheet is a page *builder*, not a
/// path) — every caller in this app hands it the **root overlay host**'s
/// controller (`crate::AppState::overlays`), so the sheet covers the shell's
/// floating bar rather than sliding up under it, and that bar goes inert for
/// free. It stays a plain parameter rather than a thread-local read because
/// `tests/ui_substrate.rs` drives this helper over a bespoke navigator of its
/// own.
///
/// # The body builder's [`SheetHandle`]
///
/// `build_body` is handed the sheet's own handle, and closing the sheet from
/// inside it is [`SheetHandle::close`] — never a bare `nav.pop()`, which would
/// be both unaccounted *and* ungated (see the [module docs](self)' "One sheet,
/// one dismissal"). The handle is minted here rather than returned because
/// every closer this app has lives in the body: a row that acts and dismisses,
/// a close button, the delayed close after a tab selection lands.
///
/// ```ignore
/// show_sheet(&state.overlays, move |sheet| {
///     let sheet = sheet.clone();
///     any(button("Done", move |_state: &mut AppState| {
///         sheet.close();
///     }))
/// });
/// ```
pub fn show_sheet<State: 'static>(
    nav: &NavigatorController<State>,
    build_body: impl Fn(&SheetHandle) -> AnyView<State> + 'static,
) {
    // The frust handle every app-driven close actually drives
    // (`GlyphSheetHandle::close`) is minted by `show_glyph_sheet` itself, and
    // isn't available until that call returns — which happens *after* it has
    // already invoked `build_body` once (its own `dismissable` peek). So the
    // closures below reach through this cell rather than capturing the handle
    // directly: it is filled in immediately below, and nothing can observe it
    // empty — the earliest an app-driven close can fire is a later frame's
    // input, long after this function has returned.
    let glyph_handle: Rc<RefCell<Option<GlyphSheetHandle>>> = Rc::new(RefCell::new(None));
    let close_cell = Rc::clone(&glyph_handle);
    // The one app-driven close, behind the one gate — the widget's own
    // staged exit (`GlyphSheetHandle::close`), not an immediate
    // `NavigatorController::pop` (see the [module docs](self)).
    let sheet = SheetHandle::new(move || {
        if let Some(handle) = close_cell.borrow().as_ref() {
            handle.close();
        }
    });
    let body_sheet = sheet.clone();
    let close_then_sheet = sheet.clone();
    let handle = show_glyph_sheet(
        nav,
        move || {
            glyph_sheet(sheet_content(build_body(&body_sheet))).on_close({
                // Frust's own composed `on_close`: the pop is enqueued first,
                // this runs second, both exactly once, on the exit's
                // completion frame — the hook `close_then` chains a
                // follow-on push through (see the [module docs](self)).
                let sheet = close_then_sheet.clone();
                move || sheet.run_pending()
            })
        },
        // The sheet answers through its body's own callbacks (which own
        // `&mut State` already); the pop itself carries nothing. What this
        // callback is for is the **latch**: the navigator delivers it exactly
        // once per pushed page, however that page went away (an action, a
        // scrim tap, a handle drag, `Escape`, a back press), so whichever way
        // the page went, a close still in flight must not land on whatever is
        // under it now.
        move |_state: &mut State, _result: PopResult| sheet.mark_closed(),
    );
    *glyph_handle.borrow_mut() = Some(handle);
}

/// The app-side content layer inside the Glyph sheet's content slot: a
/// full-content-width body, the panel gutter, and the bottom safe area.
///
/// The stretching `Column` is what makes the body full-width — Glyph lays its
/// content slot out under loose width (see the [module docs](self)), while
/// every muxr body is written expecting to span the panel. The gesture-nav /
/// home-indicator inset widens the bottom gutter rather than replacing it, so
/// the body never sits flush against the bar.
fn sheet_content<State: 'static>(body: AnyView<State>) -> AnyView<State> {
    any(safe_area(Padding(
        EdgeInsets {
            left: PANEL_PAD,
            top: 0.0,
            right: PANEL_PAD,
            bottom: PANEL_PAD,
        },
        Column(vec![body]).cross_axis(CrossAxisAlignment::Stretch),
    ))
    .top(false)
    .left(false)
    .right(false))
}

/// One live sheet's identity, and the one gate every app-driven dismissal path
/// goes through — see the [module docs](self)' "One sheet, one dismissal".
///
/// Cloneable and shared (`Rc`): the body builder is re-invoked on every
/// rebuild while the sheet is up, so each row, button and delayed close
/// captures its own clone of the *same* instance. Two sheets opened in
/// sequence are two handles, which is exactly the granularity the gate needs.
#[derive(Clone)]
pub struct SheetHandle {
    inner: Rc<SheetGuard>,
}

/// [`SheetHandle`]'s shared interior: the latch, the pending-close ticket, the
/// staged close they gate, and the follow-on
/// [`close_then`](SheetHandle::close_then) queues onto its completion.
struct SheetGuard {
    /// Whether this sheet instance has already been closed — set by the first
    /// [`SheetHandle::close`]/[`close_then`](SheetHandle::close_then) and by
    /// [`SheetHandle::mark_closed`], never cleared (a sheet is closed exactly
    /// once, then the handle is spent).
    closed: Cell<bool>,
    /// The newest delayed close's ticket. [`SheetHandle::close_after`] takes
    /// one on the way in and fires only if it is still the newest when its
    /// delay elapses — "latest wins", so a second selection extends the wait
    /// rather than stacking a second close behind the first.
    pending: Cell<u64>,
    /// The one staged close (`frust_glyph::GlyphSheetHandle::close`). Boxed
    /// (rather than the handle itself) so this type stays free of the
    /// navigator's `State` parameter — a sheet body passes it around, and
    /// every one of those call sites would otherwise carry the app's state
    /// type for no reason.
    close: Box<dyn Fn()>,
    /// [`close_then`](SheetHandle::close_then)'s follow-on, run once from
    /// [`show_sheet`]'s `on_close` wiring when the staged exit completes: the
    /// pop it lands behind is already enqueued by then (frust's own composed
    /// `on_close` pops first), so whatever this pushes always queues after
    /// it. `None` for a plain [`close`](SheetHandle::close) and for every
    /// user-driven dismissal.
    then: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl SheetHandle {
    /// Mint a handle whose one dismissal runs `close`.
    fn new(close: impl Fn() + 'static) -> Self {
        SheetHandle {
            inner: Rc::new(SheetGuard {
                closed: Cell::new(false),
                pending: Cell::new(0),
                close: Box::new(close),
                then: RefCell::new(None),
            }),
        }
    }

    /// Close the sheet: play its staged exit — **at most once per sheet
    /// instance**.
    ///
    /// Returns whether *this* call is the one that closed it. A `false` is the
    /// guarded no-op every duplicate dismissal lands in (a second row tap
    /// inside the one-rebuild window a queued pop leaves open, a close button
    /// pressed while a delayed close is pending, a timer that woke after the
    /// sheet was already dismissed), and it is ordinary rather than
    /// exceptional — hence a returned `bool` and not a `debug_assert!`, which
    /// would turn a double tap into a crash in every debug build.
    ///
    /// Plays the identical scrim-fade + panel-slide exit a scrim tap/handle
    /// drag/`Escape`/back press plays, and pops only on completion — see the
    /// [module docs](self)' "`close` plays the same staged exit". A close with
    /// a follow-on push belongs on [`close_then`](Self::close_then) instead,
    /// which chains it to land after that pop.
    pub fn close(&self) -> bool {
        if self.inner.closed.replace(true) {
            return false;
        }
        (self.inner.close)();
        true
    }

    /// Close the sheet, then run `then` once its pop actually lands — the fix
    /// for a close-then-push call site (see the [module docs](self)' "`close`
    /// plays the same staged exit"). `then` runs from [`show_sheet`]'s own
    /// `on_close` wiring, on the exit's completion frame, strictly after the
    /// pop it lands behind is already enqueued (frust's composed `on_close`
    /// pops first) — so a dialog `then` pushes never races the sheet's own
    /// pop.
    ///
    /// Same gate as [`close`](Self::close): a `false` return means this call
    /// lost the race (a duplicate row tap, a close already in flight), and
    /// `then` is dropped along with it rather than queued behind someone
    /// else's close.
    pub fn close_then(&self, then: impl FnOnce() + 'static) -> bool {
        if self.inner.closed.replace(true) {
            return false;
        }
        *self.inner.then.borrow_mut() = Some(Box::new(then));
        (self.inner.close)();
        true
    }

    /// Run and clear whatever [`close_then`](Self::close_then) queued —
    /// called once, from [`show_sheet`]'s own `on_close` wiring, when the
    /// staged exit completes. A no-op for a plain [`close`](Self::close) and
    /// for every user-driven dismissal, neither of which ever populate the
    /// pending slot `close_then` fills.
    fn run_pending(&self) {
        if let Some(then) = self.inner.then.borrow_mut().take() {
            then();
        }
    }

    /// Close the sheet `delay` from now — the "let the selection visibly move
    /// first" close (`docs/terminal.html`'s `setTimeout(closeSheet, 260)`).
    /// Total time from the call to the pop landing is `delay` plus the staged
    /// exit's own duration (see the [module docs](self)' "`close` plays the
    /// same staged exit" for that second half).
    ///
    /// **Latest wins.** Each call takes a fresh ticket, and only the newest
    /// ticket's wait actually closes: a second selection inside the window
    /// re-times the one pending close rather than queueing a second one behind
    /// it. The close itself still goes through [`close`](Self::close), so even
    /// a superseded or stale wait cannot pop twice.
    ///
    /// `async` rather than self-spawning on purpose: its one caller already
    /// runs inside a spawned task (it awaits the selection landing first), and
    /// awaiting keeps that ordering — "the selection has moved, *now* start
    /// the beat" — visible at the call site.
    pub async fn close_after(&self, delay: Duration) {
        let ticket = self.inner.pending.get() + 1;
        self.inner.pending.set(ticket);
        clean_signals::time::sleep(delay).await;
        if self.inner.pending.get() == ticket {
            self.close();
        }
    }

    /// Latch the handle without popping anything — the sheet's page has gone
    /// away through a route this app didn't drive (the widget's own scrim
    /// tap / handle drag / `Escape` / back-press exit), or through one it did.
    ///
    /// [`show_sheet`]'s completion callback is where this runs, because that
    /// is the one seam the navigator delivers exactly once per pushed page,
    /// however it went away.
    fn mark_closed(&self) {
        self.inner.closed.set(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle whose "close" only counts itself — production wires this to
    /// `frust_glyph::GlyphSheetHandle::close` (the staged exit), so
    /// counting invocations here is counting how many times that seam fired,
    /// not literal pops.
    fn counting_handle() -> (SheetHandle, Rc<Cell<usize>>) {
        let pops = Rc::new(Cell::new(0usize));
        let counted = Rc::clone(&pops);
        (
            SheetHandle::new(move || counted.set(counted.get() + 1)),
            pops,
        )
    }

    #[test]
    fn a_sheet_pops_once_however_many_times_it_is_told_to_close() {
        // The defect this gate exists for: the second close used to pop the
        // page *under* the sheet — the route itself.
        let (sheet, pops) = counting_handle();
        assert!(sheet.close(), "the first close is the one that closes it");
        assert!(!sheet.close(), "…and every later one is a no-op");
        assert!(!sheet.clone().close(), "…through any clone of the handle");
        assert_eq!(pops.get(), 1, "exactly one pop");
    }

    #[test]
    fn a_latched_handle_pops_nothing_at_all() {
        // The page went away some other way (the widget's own staged exit
        // after a scrim tap, a handle drag or a back press); a close still in
        // flight must not land on whatever is under it now.
        let (sheet, pops) = counting_handle();
        sheet.mark_closed();
        assert!(!sheet.close(), "the sheet is already gone");
        assert_eq!(pops.get(), 0);
    }

    #[test]
    fn close_then_drives_the_staged_close_and_holds_its_callback_for_run_pending() {
        // `close_then` fires the same underlying staged close as `close`
        // immediately, but its own callback waits for `run_pending` — the
        // stand-in here for `show_sheet`'s `on_close` wiring, which is what
        // actually calls it once the exit completes.
        let (sheet, pops) = counting_handle();
        let ran = Rc::new(Cell::new(false));
        let ran_write = Rc::clone(&ran);
        assert!(
            sheet.close_then(move || ran_write.set(true)),
            "the first close_then is the one that closes it"
        );
        assert_eq!(pops.get(), 1, "the staged close fires right away");
        assert!(
            !ran.get(),
            "…but the chained callback waits for run_pending"
        );
        sheet.run_pending();
        assert!(
            ran.get(),
            "…which is what runs it, on the exit's completion"
        );
        ran.set(false);
        sheet.run_pending();
        assert!(!ran.get(), "a second run_pending is an inert no-op");
    }

    #[test]
    fn a_close_then_after_the_sheet_is_already_closed_drops_its_callback() {
        // Same race `a_sheet_pops_once_however_many_times_it_is_told_to_close`
        // guards for a plain `close` — `close_then` shares the one gate, so a
        // duplicate never queues a second follow-on behind someone else's.
        let (sheet, pops) = counting_handle();
        assert!(
            sheet.close(),
            "the sheet is already closed by a plain close"
        );
        let ran = Rc::new(Cell::new(false));
        let ran_write = Rc::clone(&ran);
        assert!(
            !sheet.close_then(move || ran_write.set(true)),
            "…so close_then loses the race, same gate as a duplicate close"
        );
        assert_eq!(pops.get(), 1, "…and never drives the staged close twice");
        sheet.run_pending();
        assert!(!ran.get(), "the dropped callback never runs");
    }

    #[test]
    fn the_scrim_alpha_transcription_stays_legal() {
        assert!((0.0..=1.0).contains(&SCRIM_ALPHA));
    }
}
