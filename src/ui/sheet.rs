use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use frust::{
    AnyView, Column, CrossAxisAlignment, EdgeInsets, NavigatorController, Padding, PopResult, any,
    safe_area,
};
use frust_glyph::{GlyphSheetHandle, glyph_sheet, show_glyph_sheet};

pub const SCRIM_ALPHA: f32 = 0.6;
const PANEL_PAD: f64 = 20.0;

pub fn show_sheet<State: 'static>(
    nav: &NavigatorController<State>,
    build_body: impl Fn(&SheetHandle) -> AnyView<State> + 'static,
) {
    let glyph_handle: Rc<RefCell<Option<GlyphSheetHandle>>> = Rc::new(RefCell::new(None));
    let close_cell = Rc::clone(&glyph_handle);
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
                let sheet = close_then_sheet.clone();
                move || sheet.run_pending()
            })
        },
        move |_state: &mut State, _result: PopResult| sheet.mark_closed(),
    );
    *glyph_handle.borrow_mut() = Some(handle);
}

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

    fn run_pending(&self) {
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
