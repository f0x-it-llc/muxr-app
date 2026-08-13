//! A modal bottom sheet: [`show_sheet`] pushes one over the root overlay host.
//!
//! # The chrome is `frust::glyph`'s
//!
//! The panel, its scrim, the drag handle, the enter/exit staging and every
//! dismissal gesture belong to [`frust::glyph::glyph_sheet`] — this module is
//! the thin app-facing adapter over
//! [`show_glyph_sheet`](frust::glyph::show_glyph_sheet), which is what a muxr
//! screen calls. The app hand-composed all of that itself until the Glyph
//! catalog grew a sheet of its own; adopting it retired ~450 lines of
//! app-owned modal machinery and brought three behaviours the hand-rolled
//! version never had: **drag-to-dismiss** from the handle, an Android **back
//! press** that plays the exit staging rather than vanishing the page, and one
//! motion spec shared with the Glyph dialogs [`crate::ui::dialogs`] already
//! rides.
//!
//! What stays here is what is genuinely the *app's*: the panel gutter and
//! bottom safe-area inset a muxr body is written against ([`sheet_content`]),
//! and the dismissal gate below.
//!
//! # What the adapter adds around a body
//!
//! Glyph lays its content slot out under **loose** width, so a body that
//! shrink-wraps (a lone button, a centered `Row`) would sit at its natural
//! width in the middle of the panel. Every muxr sheet body — the picker rows,
//! the management rows, the panes-and-tabs column — is written against a
//! full-content-width column, so [`sheet_content`] tightens the width back and
//! adds the gutter/safe-area padding the Glyph panel deliberately leaves to its
//! content.
//!
//! # One sheet, one dismissal — [`SheetHandle`]
//!
//! A sheet stays interactive until its pop actually *lands*, and a pop lands a
//! rebuild after it is enqueued — so every app-driven dismissal path races
//! every other one. A second tap on a row, a close button pressed while a
//! delayed close is already pending, a timer that wakes after the sheet is
//! already gone: each of them calling [`NavigatorController::pop`] on its own
//! means the second one through pops **the page under the sheet** — whatever
//! overlay happens to be there, or (on the route navigator this app's sheets
//! used to live on) the route itself, which read to the user as the app
//! silently bouncing back a screen.
//!
//! So a sheet is not closed by whoever happens to be holding a navigator; it is
//! closed through [`SheetHandle`] — the identity [`show_sheet`] mints per sheet
//! *instance* and hands to its body builder. Every app-driven path goes through
//! [`SheetHandle::close`], which pops **at most once per instance** and is an
//! ordinary no-op after that: a duplicate close is a routine user race, not a
//! bug to panic on. [`show_sheet`]'s own completion callback latches the same
//! handle, so a close still in flight when the page goes away some other way
//! cannot pop anything either, and [`SheetHandle::close_after`] is the delayed
//! close ("let the selection visibly move first") with latest-wins re-timing on
//! top of the same gate.
//!
//! **A user-driven dismissal doesn't come through here at all**: a scrim tap,
//! a handle drag, `Escape` and a back press are the widget's own, and it pops
//! itself exactly once at the end of its exit staging (its `begin_exit` is
//! idempotent, and a dismissed sheet stays dismissed). The handle only learns
//! about it when the page is gone, through the completion latch — which is
//! what keeps a *later* stale close from popping anything.
//!
//! # `close` pops immediately; only the user's own dismissals stage
//!
//! [`SheetHandle::close`] enqueues the pop straight away rather than asking the
//! widget to play its exit first, and that ordering is load-bearing: the
//! sessions management sheet's rows (and the settings pickers) pop the sheet
//! **and** push a dialog in the same rebuild, so both ops land in one drain,
//! pop first. Deferring the pop behind a 150ms exit would land the push on top
//! of a sheet that then popped the dialog it opened. It also reads better —
//! a sheet sliding out under a dialog fading in is two competing animations
//! rather than one replacement, the same call `frust::glyph::dialog`'s own
//! action buttons make.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use frust::glyph::{glyph_sheet, show_glyph_sheet};
use frust::{
    AnyView, Column, CrossAxisAlignment, EdgeInsets, NavigatorController, Padding, PopResult, any,
    safe_area,
};

/// Scrim opacity behind a modal, transcribed from the Glyph catalog's own
/// barrier constant (private to `frust::glyph`'s dialog and sheet, which pick
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
    let pop_nav = nav.clone();
    // The one app-driven pop, behind the one gate.
    let sheet = SheetHandle::new(move || pop_nav.pop());
    let body_sheet = sheet.clone();
    show_glyph_sheet(
        nav,
        move || glyph_sheet(sheet_content(build_body(&body_sheet))),
        // The sheet answers through its body's own callbacks (which own
        // `&mut State` already); the pop itself carries nothing. What this
        // callback is for is the **latch**: the navigator delivers it exactly
        // once per pushed page, however that page went away (an action, a
        // scrim tap, a handle drag, `Escape`, a back press), so whichever way
        // the page went, a close still in flight must not land on whatever is
        // under it now.
        move |_state: &mut State, _result: PopResult| sheet.mark_closed(),
    );
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

/// [`SheetHandle`]'s shared interior: the latch, the pending-close ticket, and
/// the accounted pop they gate.
struct SheetGuard {
    /// Whether this sheet instance has already been closed — set by the first
    /// [`SheetHandle::close`] and by [`SheetHandle::mark_closed`], never
    /// cleared (a sheet is closed exactly once, then the handle is spent).
    closed: Cell<bool>,
    /// The newest delayed close's ticket. [`SheetHandle::close_after`] takes
    /// one on the way in and fires only if it is still the newest when its
    /// delay elapses — "latest wins", so a second selection extends the wait
    /// rather than stacking a second close behind the first.
    pending: Cell<u64>,
    /// The one pop (`NavigatorController::pop`). Boxed (rather than the
    /// controller itself) so the handle stays free of the navigator's `State`
    /// parameter — a sheet body passes it around, and every one of those call
    /// sites would otherwise carry the app's state type for no reason.
    close: Box<dyn Fn()>,
}

impl SheetHandle {
    /// Mint a handle whose one dismissal runs `close`.
    fn new(close: impl Fn() + 'static) -> Self {
        SheetHandle {
            inner: Rc::new(SheetGuard {
                closed: Cell::new(false),
                pending: Cell::new(0),
                close: Box::new(close),
            }),
        }
    }

    /// Close the sheet: enqueue the pop — **at most once per sheet
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
    /// Pops immediately rather than asking the widget to play its exit first —
    /// see the [module docs](self)' "`close` pops immediately".
    pub fn close(&self) -> bool {
        if self.inner.closed.replace(true) {
            return false;
        }
        (self.inner.close)();
        true
    }

    /// Close the sheet `delay` from now — the "let the selection visibly move
    /// first" close (`docs/terminal.html`'s `setTimeout(closeSheet, 260)`).
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

    /// A handle whose "pop" only counts itself — a bare
    /// `NavigatorController::pop` is what [`show_sheet`] installs here, so
    /// counting invocations *is* counting pops.
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
    fn the_scrim_alpha_transcription_stays_legal() {
        assert!((0.0..=1.0).contains(&SCRIM_ALPHA));
    }
}
