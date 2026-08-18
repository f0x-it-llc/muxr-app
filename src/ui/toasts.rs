use std::cell::Cell;

use clean_signals::Failure;
use frust::{AnyView, Get, GetUntracked, RwSignal, Update, any};
use frust_glyph::toast_host;

use crate::failure::AppFailure;

#[derive(Clone, Copy)]
pub struct FailureToasts {
    queue: RwSignal<Vec<String>>,
}

thread_local! {
    /// The one [`FailureToasts`] per UI thread — see [`app_toasts`].
    static TOASTS: Cell<Option<FailureToasts>> = const { Cell::new(None) };
}

/// The app's toast handle, created once per thread and reused afterwards.
///
/// Self-healing: a cached handle whose signal was disposed with its owner (a
/// headless test that dropped its ambient `Owner` and re-entered) reads back
/// `None` from `try_get_untracked` and is replaced rather than handed out
/// dead.
pub fn app_toasts() -> FailureToasts {
    TOASTS.with(|cell| {
        if let Some(toasts) = cell.get()
            && toasts.queue.try_get_untracked().is_some()
        {
            return toasts;
        }
        let toasts = FailureToasts {
            queue: RwSignal::new(Vec::new()),
        };
        cell.set(Some(toasts));
        toasts
    })
}

impl FailureToasts {
    pub fn raise(&self, failure: &AppFailure) {
        self.show(failure.user_message());
    }

    pub fn show(&self, message: impl Into<String>) {
        self.queue.update(|queue| queue.push(message.into()));
    }

    pub fn pending(&self) -> Vec<String> {
        self.queue.get()
    }

    pub fn pending_untracked(&self) -> Vec<String> {
        self.queue.get_untracked()
    }

    pub fn clear(&self) {
        self.queue.update(Vec::clear);
    }
}

pub fn toast_layer<State: 'static>() -> AnyView<State> {
    any(toast_host(app_toasts().pending()))
}
