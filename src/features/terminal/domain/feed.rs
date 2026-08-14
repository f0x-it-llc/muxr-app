//! [`TerminalFeed`] — the byte-level port between a terminal *session* and the grid
//! that paints it.
//!
//! Deliberately **not** part of
//! [`repositories::TerminalRepository`](super::repositories): that contract speaks
//! the screen's *model* (workspaces, tabs, transcript lines) and answers once per
//! call, while this is a live, unstructured byte stream in both directions. Folding
//! a 30 Hz byte pump into it would make every implementation carry a pump it does
//! not want.
//!
//! Four decisions the shape encodes:
//!
//! 1. **Pull, not push.** The pump owns the cadence, because the repaint cap
//!    (`presentation::term`'s `REPAINT_HZ`) is the app's only defence against a
//!    signal write per received chunk.
//! 2. **Drain-all-due per call**, never one chunk per call: a late wake catches up
//!    on everything queued, so no input byte is dropped — only the *repaint* is
//!    coalesced.
//! 3. **A sink closure, not a returned buffer**, so a feed hands out borrowed slices
//!    straight from its own storage with no per-wake allocation.
//! 4. **Infallible.** A transport error belongs to whatever owns the session; a feed
//!    that has *stopped* reports it through [`TerminalFeed::is_open`], a state.
//!
//! Framework-free like the rest of `domain`: this port deals only in bytes.
//! [`TerminalFeed::send_mouse`] is structured rather than folded into `send_input`'s
//! raw bytes, because the wire carries it as its own `ClientFrame.mouse` variant,
//! zero-based in the grid `resize` establishes, not an ANSI mouse-reporting escape.
//! `&self`, not `&mut self`: nothing a shipping implementation does to send one
//! needs exclusive access, and a feed with nothing to route it to inherits the
//! default no-op.

/// Wheel direction [`TerminalFeed::send_mouse`] sends — mirrors the wire's
/// `MouseKind`, which also reserves press/drag values for later; nothing on this
/// screen produces anything but a wheel notch yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelKind {
    /// One wheel notch up.
    Up,
    /// One wheel notch down.
    Down,
}

/// A live, bidirectional stream of terminal bytes.
///
/// See the [module docs](self) for why the shape is what it is.
pub trait TerminalFeed {
    /// Hand every byte that has arrived (or, for a replay, come due) since the last
    /// call to `sink`, and report how many separate writes were delivered — what the
    /// pump gates its single generation write on (`0` means nothing repaints). It
    /// counts *writes*, not bytes, deliberately: a feed is free to coalesce or split
    /// its buffers. Must not block: it runs on the UI thread, inside the pump's wake.
    fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize;

    /// Send `bytes` to the session (keystrokes, paste, control sequences).
    fn send_input(&mut self, bytes: &[u8]);

    /// Tell the session its window is now `cols` x `rows` cells. Column-major
    /// argument order, matching every other geometry call in this app — `vt100`'s own
    /// API takes `(rows, cols)`, so the presentation layer swaps at that boundary and
    /// nowhere else.
    fn resize(&mut self, cols: u16, rows: u16);

    /// Whether this feed can still produce output — the pump's exit condition.
    /// Defaults to `true`: a feed with no natural end (a live session) says nothing.
    fn is_open(&self) -> bool {
        true
    }

    /// Send a wheel-scroll mouse event at `(col, row)` — zero-based cells in
    /// this feed's own grid, routed by the multiplexer to whichever pane sits
    /// under them. Defaults to a no-op.
    fn send_mouse(&self, _kind: WheelKind, _col: u32, _row: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A feed overriding only the required methods compiles unmodified, and
    /// its inherited `send_mouse` is a genuine no-op.
    struct BareFeed;

    impl TerminalFeed for BareFeed {
        fn drain_output(&mut self, _sink: &mut dyn FnMut(&[u8])) -> usize {
            0
        }

        fn send_input(&mut self, _bytes: &[u8]) {}

        fn resize(&mut self, _cols: u16, _rows: u16) {}
    }

    #[test]
    fn send_mouse_defaults_to_a_no_op() {
        let feed = BareFeed;
        feed.send_mouse(WheelKind::Up, 3, 7);
        feed.send_mouse(WheelKind::Down, 0, 0);
    }
}
