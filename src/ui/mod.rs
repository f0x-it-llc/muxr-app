//! Shared, cross-feature widget helpers — the shape `examples/huddle`'s
//! `ui/` (toast, sheet, scaffold, fill_box, …) suggests this grows into as
//! muxr needs more app-wide chrome shared by more than one feature.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`backdrop`] | Full-bleed page background the Connect screen and splash overlay paint |
//! | [`dialogs`] | Confirm / name-input modal helpers over `frust_glyph`'s dialog |
//! | [`paint`] | Cross-feature paint-primitive tokens (hairline, full-pill radius, status-dot glow, dashed-border step) shared by every feature's hand-rolled chrome |
//! | [`palette`] | The app's one theme-derived colour vocabulary, union of the four per-feature `Palette`s — not yet adopted |
//! | [`sheet`] | The modal bottom-sheet adapter over `frust_glyph`'s own sheet (`show_sheet`, over this module's own [`SheetHandle`]) |
//! | [`side_sheet`] | The modal side-sheet adapter over `frust_glyph`'s own side sheet (`show_side_sheet`, the x-axis sibling of [`sheet::show_sheet`], over the same [`SheetHandle`]) |
//! | [`spacing`] | Named spacing tokens on the measured de-facto scale — not yet adopted |
//! | [`splash`] | The cold-start splash sequence (design reference: `docs/splash.html`/`docs/muxr-app-icons`) |
//! | [`toasts`] | The failure→toast queue handle and the toast layer the shell mounts |
//! | [`topbar`] | The app's bar furniture: the brand row and icon slot Glyph's own app bar wears, plus `icon_button` and the page clearance |

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

pub mod backdrop;
pub mod dialogs;
pub mod paint;
pub mod palette;
pub mod sheet;
pub mod side_sheet;
pub mod spacing;
pub mod splash;
pub mod toasts;
pub mod topbar;

/// The one-shot dismissal gate [`sheet::show_sheet`] and
/// [`side_sheet::show_side_sheet`] each mint one of per instance: a `close`/
/// [`close_then`](Self::close_then) fires **at most once**, so a duplicate
/// close (a second row tap inside the close beat, a scrim tap racing a
/// commit, …) cannot pop the route or page underneath the sheet a second
/// time. Shared here — rather than declared once per adapter — because both
/// modal shapes wire the identical contract onto their own catalog widget's
/// staged exit.
#[derive(Clone)]
pub struct SheetHandle {
    inner: Rc<SheetGuard>,
}

/// [`SheetHandle`]'s shared interior: the latch, the pending-close ticket, the
/// staged close they gate, and the follow-on
/// [`close_then`](SheetHandle::close_then) queues onto its completion.
struct SheetGuard {
    closed: Cell<bool>,
    pending: Cell<u64>,
    close: Box<dyn Fn()>,
    then: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl SheetHandle {
    /// Mint a fresh handle whose staged close is `close` — an adapter's own
    /// seam onto its catalog widget's programmatic-close handle
    /// (`GlyphSheetHandle::close`/`GlyphSideSheetHandle::close`).
    pub(crate) fn new(close: impl Fn() + 'static) -> Self {
        SheetHandle {
            inner: Rc::new(SheetGuard {
                closed: Cell::new(false),
                pending: Cell::new(0),
                close: Box::new(close),
                then: RefCell::new(None),
            }),
        }
    }

    pub fn close(&self) -> bool {
        if self.inner.closed.replace(true) {
            return false;
        }
        (self.inner.close)();
        true
    }

    pub fn close_then(&self, then: impl FnOnce() + 'static) -> bool {
        if self.inner.closed.replace(true) {
            return false;
        }
        *self.inner.then.borrow_mut() = Some(Box::new(then));
        (self.inner.close)();
        true
    }

    pub(crate) fn run_pending(&self) {
        if let Some(then) = self.inner.then.borrow_mut().take() {
            then();
        }
    }

    pub async fn close_after(&self, delay: Duration) {
        let ticket = self.inner.pending.get() + 1;
        self.inner.pending.set(ticket);
        clean_signals::time::sleep(delay).await;
        if self.inner.pending.get() == ticket {
            self.close();
        }
    }

    pub(crate) fn mark_closed(&self) {
        self.inner.closed.set(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle whose "close" only counts itself — production wires this to
    /// `frust_glyph::GlyphSheetHandle::close`/`GlyphSideSheetHandle::close`
    /// (each adapter's own staged exit), so counting invocations here is
    /// counting how many times that seam fired, not literal pops.
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
        // stand-in here for an adapter's own `on_close` wiring, which is what
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
}
