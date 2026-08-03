//! [`TerminalFeed`] — the byte-level port between a terminal *session* and the
//! grid that paints it.
//!
//! Deliberately **not** part of [`repositories::TerminalRepository`](super::repositories):
//! that contract speaks the screen's *model* (workspaces, tabs, mock transcript
//! lines) and answers once per call; this one is a live, unstructured byte
//! stream in both directions. Folding a 30 Hz byte pump into a repository whose
//! other methods are one-shot use-case calls would make every implementation of
//! it carry a pump it doesn't want, so the port is additive and separate.
//!
//! # Three consumers, one shape
//!
//! | Consumer | What it needs |
//! |---|---|
//! | the grid widget's feed pump (`presentation::term`) | pull every byte that is due *right now*, on the pump's own cadence |
//! | the demo replay (`data::feeds::DemoFeed`) | replay a checked-in fixture at its recorded rate, and echo typed bytes back |
//! | a future `muxrd` gRPC session | hand over whatever the stream delivered since the last wake; accept keystrokes and a window size |
//!
//! Five decisions the shape encodes, each of which the alternative would break
//! for one of those three:
//!
//! 1. **Pull, not push.** A feed never calls back into the UI on its own
//!    schedule. The pump owns the cadence, because the repaint cap
//!    (`presentation::term`'s `REPAINT_HZ`) is the app's only defence against a
//!    signal write per received chunk — see that module's docs. A push feed
//!    would move that decision into the transport, where a firehose of small
//!    gRPC messages would drive one whole-app rebuild each.
//! 2. **Drain-all-due per call**, never one chunk per call. A late wake must
//!    catch up on everything queued rather than skipping or dribbling it: no
//!    input byte is ever dropped or delayed past its arrival, only the *repaint*
//!    is coalesced.
//! 3. **A sink closure, not a returned buffer.** `&mut dyn FnMut(&[u8])` lets a
//!    feed hand out borrowed slices straight from its own storage (the demo
//!    feed's `include_bytes!`ed fixture, a gRPC frame still owned by its
//!    decoder) with no per-wake allocation on the hottest path in the app.
//! 4. **Infallible.** A transport error is not an output event: it belongs to
//!    whatever owns the session (a controller mapping it into
//!    [`AppFailure`](crate::failure::AppFailure) and a toast), not to the 30 Hz
//!    byte path, which must not grow an error branch it would only ever
//!    `let _ =`. A feed that has *stopped* reports it through
//!    [`TerminalFeed::is_open`], which is a state, not a failure.
//! 5. **`send_input`/`resize` belong here from the start**, even though nothing
//!    calls them until the input and cutover tasks land. They are the same
//!    session, in the other direction; discovering them later would mean
//!    re-opening every implementation plus the pump that owns the session
//!    handle.
//!
//! Framework-free like the rest of `domain`: no `frust::`, no signals, no
//! `vt100` (the emulator is the *presentation* layer's business — this port
//! deals only in bytes, so a feed can be tested without one).

/// A live, bidirectional stream of terminal bytes.
///
/// See the [module docs](self) for why the shape is what it is.
pub trait TerminalFeed {
    /// Hand every byte that has arrived (or, for a replay, come due) since the
    /// last call to `sink`, and report how many separate writes were delivered.
    ///
    /// The return value is what the pump gates its single generation write on:
    /// `0` means nothing arrived, so nothing repaints. It counts *writes*, not
    /// bytes, deliberately — a feed is free to coalesce or split its buffers,
    /// and no caller may treat the number as a byte count.
    ///
    /// Must not block: it runs on the UI thread, inside the pump's wake.
    fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize;

    /// Send `bytes` to the session (keystrokes, paste, control sequences).
    ///
    /// Unwired until the terminal input task lands; part of the port now
    /// because it is the same session in the other direction (module docs,
    /// decision 5).
    fn send_input(&mut self, bytes: &[u8]);

    /// Tell the session its window is now `cols` x `rows` cells.
    ///
    /// Column-major argument order (`cols, rows`) matching every other
    /// geometry call in this app — note that `vt100`'s own API takes
    /// `(rows, cols)`, so the presentation layer swaps at that boundary and
    /// nowhere else.
    fn resize(&mut self, cols: u16, rows: u16);

    /// Whether this feed can still produce output.
    ///
    /// The pump's exit condition: `false` means the replay is exhausted or the
    /// session closed, and the pump stops waking. Defaults to `true` — a feed
    /// with no natural end (a live session) says nothing.
    fn is_open(&self) -> bool {
        true
    }
}
