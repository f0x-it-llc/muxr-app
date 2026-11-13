//! Cross-feature widget helpers and app-wide chrome shared by more than one feature.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

pub mod agent_marks;
pub mod backdrop;
pub mod dialogs;
pub mod paint;
pub mod palette;
pub mod recency;
pub mod sheet;
pub mod side_sheet;
pub mod spacing;
pub mod splash;
pub mod toasts;
pub mod topbar;

/// One-shot dismissal gate shared by both sheet adapters: a close fires at most once, so a
/// duplicate close cannot pop the route underneath the sheet.
#[derive(Clone)]
pub struct SheetHandle {
    inner: Rc<SheetGuard>,
}

struct SheetGuard {
    closed: Cell<bool>,
    pending: Cell<u64>,
    close: Box<dyn Fn()>,
    then: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl SheetHandle {
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
        let (sheet, pops) = counting_handle();
        assert!(sheet.close(), "the first close is the one that closes it");
        assert!(!sheet.close(), "…and every later one is a no-op");
        assert!(!sheet.clone().close(), "…through any clone of the handle");
        assert_eq!(pops.get(), 1, "exactly one pop");
    }

    #[test]
    fn a_latched_handle_pops_nothing_at_all() {
        let (sheet, pops) = counting_handle();
        sheet.mark_closed();
        assert!(!sheet.close(), "the sheet is already gone");
        assert_eq!(pops.get(), 0);
    }

    #[test]
    fn close_then_drives_the_staged_close_and_holds_its_callback_for_run_pending() {
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
