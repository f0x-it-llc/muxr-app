//! [`TerminalFeed`] — the byte-level port between a terminal session and the grid that paints it.
//!
//! Not part of [`repositories::TerminalRepository`](super::repositories), which speaks the screen's
//! model and answers once per call: this is a live, unstructured byte stream both ways.
//!
//! - **Pull, not push.** The pump owns the cadence — the repaint cap (`presentation::term`'s
//!   `REPAINT_HZ`) is the only defence against a signal write per received chunk.
//! - **Drain-all-due per call**, not one chunk per call: a late wake catches up on everything
//!   queued, so no input byte drops — only the repaint is coalesced.
//! - **Sink closure, not a returned buffer**: a feed hands out borrowed slices from its own
//!   storage with no per-wake allocation.
//! - **Infallible.** A transport error belongs to whatever owns the session; a stopped feed
//!   reports it through [`TerminalFeed::is_open`].
//!
//! [`TerminalFeed::send_mouse`] is structured, not folded into `send_input`'s raw bytes: the wire
//! carries it as its own `ClientFrame.mouse` variant, zero-based in the grid `resize` establishes.
//! `&self`, not `&mut self`: sending one needs no exclusive access.

/// Where a live feed's link stands — the link strip's and rail header's vocabulary. Plain
/// state the screen polls, not a signal; the reconnect machinery behind it is `data`'s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkStatus {
    /// Constructed; no stream opened yet.
    #[default]
    Idle,
    /// An attach attempt is in flight, or a backoff step is being waited out.
    Reconnecting,
    /// The stream is open and delivering.
    Connected,
    /// Parked: the schedule ran out or the credential expired; the screen's retry resets it.
    Disconnected,
    /// The server said the session exited — nothing will arrive again.
    Closed,
}

/// Wheel direction [`TerminalFeed::send_mouse`] sends — mirrors the wire's `MouseKind`,
/// which also reserves press/drag values; this screen only produces a wheel notch yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelKind {
    /// One wheel notch up.
    Up,
    /// One wheel notch down.
    Down,
}

/// A live, bidirectional stream of terminal bytes; the [module docs](self) explain its shape.
pub trait TerminalFeed {
    /// Hand every byte that arrived (or came due, for a replay) since the last call to `sink`;
    /// returns the write count the pump gates its single repaint on (`0` = no repaint). Counts
    /// writes, not bytes — a feed may coalesce or split freely. Must not block (UI thread, inside
    /// the pump's wake).
    fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize;

    /// Send `bytes` to the session (keystrokes, paste, control sequences).
    fn send_input(&mut self, bytes: &[u8]);

    /// Tell the session its window is now `cols` x `rows` cells — column-major, matching every
    /// other geometry call in this app (`vt100`'s own API takes `(rows, cols)`; the presentation
    /// layer swaps at that boundary and nowhere else).
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
