use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clean_signals::Failure;
use frust::{Get, GetUntracked, RwSignal, Update, View};
use frust_glyph::toast_host;

use crate::failure::AppFailure;

#[derive(Clone, Copy)]
pub struct FailureToasts {
    queue: RwSignal<Vec<String>>,
    /// When each message was last raised; pruned on every raise.
    recent: RwSignal<Vec<(String, Instant)>>,
}

/// A toast whose message equals one raised this recently is dropped: a failure that
/// repeats on a cadence (a poll, a retry) must read as one event, not a stack of them.
pub const DEDUPE_WINDOW: Duration = Duration::from_secs(2);

static CLOCK_AHEAD_MS: AtomicU64 = AtomicU64::new(0);

/// `pub` rather than `#[cfg(test)]` because `tests/*.rs` link the normal build.
pub fn advance_toast_clock_for_tests(by: Duration) {
    CLOCK_AHEAD_MS.fetch_add(
        u64::try_from(by.as_millis()).unwrap_or(u64::MAX),
        Ordering::SeqCst,
    );
}

fn toast_now() -> Instant {
    Instant::now() + Duration::from_millis(CLOCK_AHEAD_MS.load(Ordering::SeqCst))
}

thread_local! {
    static TOASTS: Cell<Option<FailureToasts>> = const { Cell::new(None) };
}

/// Created once per thread; a cached handle whose signal was disposed with its owner is
/// replaced rather than handed out dead.
pub fn app_toasts() -> FailureToasts {
    TOASTS.with(|cell| {
        if let Some(toasts) = cell.get()
            && toasts.queue.try_get_untracked().is_some()
        {
            return toasts;
        }
        let toasts = FailureToasts {
            queue: RwSignal::new(Vec::new()),
            recent: RwSignal::new(Vec::new()),
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
        let message = message.into();
        let now = toast_now();
        let mut duplicate = false;
        self.recent.update(|recent| {
            recent.retain(|(_, at)| now.saturating_duration_since(*at) < DEDUPE_WINDOW);
            duplicate = recent.iter().any(|(seen, _)| *seen == message);
            if !duplicate {
                recent.push((message.clone(), now));
            }
        });
        if !duplicate {
            self.queue.update(|queue| queue.push(message));
        }
    }

    pub fn pending(&self) -> Vec<String> {
        self.queue.get()
    }

    pub fn pending_untracked(&self) -> Vec<String> {
        self.queue.get_untracked()
    }

    pub fn clear(&self) {
        self.queue.update(Vec::clear);
        self.recent.update(Vec::clear);
    }
}

pub fn toast_layer<State: 'static>() -> impl View<State> {
    toast_host(app_toasts().pending())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identical_toast_inside_the_window_is_dropped_then_shown_after_it() {
        let toasts = app_toasts();
        toasts.clear();
        toasts.show("same");
        toasts.show("same");
        toasts.show("other");
        assert_eq!(toasts.pending_untracked(), vec!["same", "other"]);
        advance_toast_clock_for_tests(DEDUPE_WINDOW + Duration::from_millis(1));
        toasts.show("same");
        assert_eq!(toasts.pending_untracked(), vec!["same", "other", "same"]);
        toasts.clear();
    }
}
