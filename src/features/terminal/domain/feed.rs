//! The live byte stream between a session and its grid. Pulled, not pushed: the pump owns the
//! cadence and drains everything due per wake. Infallible: a stopped feed reports through
//! [`TerminalFeed::is_open`].

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkStatus {
    #[default]
    Idle,
    Reconnecting,
    Connected,
    /// The schedule ran out or the credential expired; the screen's retry resets it.
    Disconnected,
    /// The session exited; nothing arrives again.
    Closed,
}

/// Mirrors the wire's `MouseKind`, which also reserves press and drag values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelKind {
    Up,
    Down,
}

pub trait TerminalFeed {
    /// Returns the write count (not bytes) the pump gates its repaint on; `0` means no repaint.
    /// Must not block: it runs on the UI thread inside the pump's wake.
    fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize;

    fn send_input(&mut self, bytes: &[u8]);

    /// Column-major; `vt100` takes `(rows, cols)`, swapped at the presentation boundary only.
    fn resize(&mut self, cols: u16, rows: u16);

    fn is_open(&self) -> bool {
        true
    }

    /// `(col, row)` are zero-based cells in the grid `resize` established.
    fn send_mouse(&self, _kind: WheelKind, _col: u32, _row: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

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
