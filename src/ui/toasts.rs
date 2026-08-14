//! The app-wide failure→toast substrate: a small queue handle plus the
//! [`frust::glyph::toast_host`] layer [`crate::shell::root_view`] mounts over
//! the navigator.
//!
//! A screen never builds a toast itself — it hands [`FailureToasts`] an
//! [`AppFailure`] and the bridge enqueues that failure's **user-facing**
//! message ([`clean_signals::Failure::user_message`]), never the internal
//! detail the variant carries for logs. That one-way mapping is the whole
//! point of the bridge: there is no path from app code to a raw `Display`
//! string on screen.
//!
//! **Why a thread-local, not `provide_context`**: [`crate::shell::root_view`]
//! runs inside `Component::build`, re-entered every rebuild, so
//! `provide_context` there would re-provide the handle per frame under the
//! build-scope owner rather than once under the root owner. The handle lives
//! in a process `thread_local!` instead, reached via [`app_toasts`] — a
//! self-healing cached-signal: a cache entry whose reactive `Owner` was
//! disposed (a headless test re-entering after teardown) is detected and
//! transparently recreated. Per-**thread** also isolates each headless test,
//! since `cargo test` runs every test on its own thread.
//!
//! **Queue policy.** [`frust::glyph::toast_host`] is a controlled component:
//! it takes the app's whole FIFO request log as a snapshot every rebuild and
//! advances its own cursor through it, showing exactly one toast at a time.
//! The log therefore only ever grows — the framework's documented v1
//! limitation — so [`FailureToasts::clear`] exists to trim it.

use std::cell::Cell;

use clean_signals::Failure;
use frust::glyph::toast_host;
use frust::{AnyView, Get, GetUntracked, RwSignal, Update, any};

use crate::failure::AppFailure;

/// A cheap, `Copy` handle to the app's toast request queue — muxr's answer to
/// `examples/huddle`'s `ToastController`, narrowed to the one thing this app
/// raises toasts for (a failure) plus a plain informational message.
///
/// Obtain one with [`app_toasts`]; never construct the queue signal yourself,
/// or the layer [`crate::shell::root_view`] mounts would be reading a
/// different one.
#[derive(Clone, Copy)]
pub struct FailureToasts {
    /// The FIFO request log `toast_host` snapshots every rebuild.
    queue: RwSignal<Vec<String>>,
}

thread_local! {
    /// The one [`FailureToasts`] per UI thread — see [`app_toasts`] and the
    /// [module docs](self).
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
    /// Surface `failure` to the user: enqueues its
    /// [`user_message`](clean_signals::Failure::user_message), never its
    /// internal detail (which stays for `Display`/logs).
    pub fn raise(&self, failure: &AppFailure) {
        self.show(failure.user_message());
    }

    /// Enqueue a plain informational message (a success confirmation, say).
    /// Failure paths go through [`raise`](Self::raise) instead, so the
    /// user-message mapping is never re-derived at a call site.
    pub fn show(&self, message: impl Into<String>) {
        self.queue.update(|queue| queue.push(message.into()));
    }

    /// This rebuild's request snapshot, as a **tracked** read — the value
    /// [`toast_layer`] feeds the host, and what makes a later
    /// [`show`](Self::show) wake a rebuild.
    pub fn pending(&self) -> Vec<String> {
        self.queue.get()
    }

    /// The request log read **without** tracking — for tests and assertions,
    /// never for a value a `build` return depends on (`docs/CODE_STANDARDS.md`'s
    /// tracked-read rule).
    pub fn pending_untracked(&self) -> Vec<String> {
        self.queue.get_untracked()
    }

    /// Drop every request from the log (see the [module docs](self)'s queue
    /// policy).
    ///
    /// Safe while **idle**: the host clamps its own cursor to the shortened
    /// snapshot on the next rebuild, so a message enqueued after a clear plays
    /// from the top. Clearing while a toast is mid-flight also drops any
    /// request queued behind it — trim between bursts, not during one.
    pub fn clear(&self) {
        self.queue.update(Vec::clear);
    }
}

/// The toast layer `crate::shell::root_view` stacks over the navigator: the
/// Glyph [`toast_host`] reading this thread's [`FailureToasts`] queue.
///
/// Generic over `State` because the host itself is (it renders no app state) —
/// which also lets a headless test mount the layer over its own state type.
pub fn toast_layer<State: 'static>() -> AnyView<State> {
    any(toast_host(app_toasts().pending()))
}
