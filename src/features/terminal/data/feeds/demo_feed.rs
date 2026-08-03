//! [`DemoFeed`] — the mock-era terminal byte source: a checked-in build-log
//! capture, replayed at its recorded rate, plus a PTY-style echo of anything
//! sent back into it.
//!
//! # The fixture is data, never generated at runtime
//!
//! [`FIXTURE`] is `benchmarks/harness/fixtures/terminal/build-log.chunks` from
//! the Frust checkout, copied **bit for bit** (sha256 [`FIXTURE_SHA256`],
//! verified by this module's own test) and `include_bytes!`d rather than read
//! from disk: Android's asset layout and iOS's bundle layout would each need
//! their own loader, and a run could then silently replay a stale or missing
//! file. It is the same 30 Hz, 900-chunk, 80x45 stream the S9 benchmark
//! measured this renderer against, so what the app paints on a device is
//! directly comparable with the numbers that made a pure-frust grid the
//! verdict.
//!
//! Framing, per that fixture directory's own `README.md`:
//!
//! ```text
//! build-log.chunks   repeat [u32 little-endian length][length bytes]
//!                    no header, no trailer, no padding
//! replay             emit chunk i at t = i * (1000 / HZ) ms from start
//! ```
//!
//! # Time is injected, never read behind the caller's back
//!
//! The port ([`TerminalFeed`]) is deliberately time-free — "what is due" is the
//! feed's own business — so this feed owns a [`Cadence`]: real elapsed wall
//! time in the app, or a test-driven cursor ([`DemoFeed::manual`]) so the whole
//! replay schedule is unit-testable without sleeping. Same seam shape as the
//! measure-closure the grid's geometry solver takes.
//!
//! # Echo mode
//!
//! A real PTY echoes what you type before the program under it answers.
//! [`DemoFeed::send_input`] therefore appends its bytes to the output stream
//! (on by default; [`DemoFeed::with_echo`] turns it off), which is what makes
//! "the keys I pressed appeared on screen, in order, byte for byte" checkable
//! on a device long before a real session exists.

use std::time::{Duration, Instant};

use crate::features::terminal::domain::feed::TerminalFeed;

/// The embedded fixture: the S9 `build-log` profile, byte-identical to
/// `benchmarks/harness/fixtures/terminal/build-log.chunks` in the Frust
/// checkout (see the [module docs](self)).
pub const FIXTURE: &[u8] = include_bytes!("../fixtures/build-log.chunks");

/// sha256 of [`FIXTURE`], transcribed from that fixture's own
/// `build-log.json` manifest. The tripwire against a re-generated or truncated
/// copy: a fixture that changed is a run that is no longer comparable with the
/// S9 measurements, which is exactly the kind of drift a checked-in binary
/// blob hides.
pub const FIXTURE_SHA256: &str = "f6b62258cdbe1b9f4758c22f3eee8dbe07793dab26e0d1f302f1c39ec93776b1";

/// Chunk emission rate, from the fixture's manifest (`"hz": 30`).
pub const HZ: f64 = 30.0;

/// Chunks in [`FIXTURE`], from the fixture's manifest (`"chunk_count": 900`) —
/// 30 seconds of build log. Asserted against the embedded bytes by this
/// module's tests.
pub const CHUNK_COUNT: usize = 900;

/// The grid the fixture was captured against (`"cols": 80, "rows": 45`).
///
/// Not a constraint on the widget — the app derives its own cols x rows from
/// the available box (see `presentation::term::geometry`) and resizes the
/// emulator to match, exactly as a real session would. Recorded because a
/// narrower grid wraps these lines, and a reader comparing what the phone
/// paints against S9's screenshots needs to know why.
pub const FIXTURE_COLS: u16 = 80;
/// See [`FIXTURE_COLS`].
pub const FIXTURE_ROWS: u16 = 45;

/// Milliseconds between consecutive chunks (`1000 / HZ`).
const INTERVAL_MS: f64 = 1000.0 / HZ;

/// Read the next `[u32 le length][length bytes]` chunk at `cursor`, advancing
/// it past the payload.
///
/// `None` on a clean end **or** a truncated tail: a bad fixture must stop the
/// replay, never panic mid-frame. A rejected read leaves `cursor` untouched.
pub fn next_chunk<'a>(chunks: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    let header_end = cursor.checked_add(4)?;
    let header = chunks.get(*cursor..header_end)?;
    let len = u32::from_le_bytes(header.try_into().expect("4-byte slice")) as usize;
    let payload_end = header_end.checked_add(len)?;
    let payload = chunks.get(header_end..payload_end)?;
    *cursor = payload_end;
    Some(payload)
}

/// Count the chunks in a stream and report the byte offset reached — the
/// fixture-integrity helper the tests assert with (`offset == chunks.len()`
/// means no trailing garbage).
pub fn walk_chunks(chunks: &[u8]) -> (usize, usize) {
    let mut cursor = 0usize;
    let mut count = 0usize;
    while next_chunk(chunks, &mut cursor).is_some() {
        count += 1;
    }
    (count, cursor)
}

/// [`DemoFeed`]'s time source (see the [module docs](self)).
enum Cadence {
    /// Wall-clock elapsed since construction — the app.
    Real(Instant),
    /// A cursor a test advances by hand.
    Manual(Duration),
}

impl Cadence {
    fn elapsed(&self) -> Duration {
        match self {
            Cadence::Real(start) => start.elapsed(),
            Cadence::Manual(elapsed) => *elapsed,
        }
    }
}

/// The checked-in build-log replay, plus a PTY-style echo.
///
/// See the [module docs](self); implements
/// [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed).
pub struct DemoFeed {
    /// Byte offset of the next unread chunk header.
    cursor: usize,
    /// Index of the next chunk — drives the `i * (1000 / HZ)` schedule.
    next_index: usize,
    clock: Cadence,
    /// Whether [`TerminalFeed::send_input`] echoes back into the output stream.
    echo: bool,
    /// Echoed bytes awaiting the next drain. One `Vec`, reused: a keystroke
    /// must not allocate.
    echoed: Vec<u8>,
    /// The last size [`TerminalFeed::resize`] was told about — the test hook
    /// standing in for a `SIGWINCH`/gRPC resize until a real session exists.
    last_resize: Option<(u16, u16)>,
    /// Diagnostics: chunks and payload bytes actually handed out.
    fed_chunks: usize,
    fed_bytes: usize,
}

impl DemoFeed {
    /// A feed positioned at chunk 0, replaying against the wall clock from
    /// **now**, with echo on.
    pub fn new() -> Self {
        Self::with_clock(Cadence::Real(Instant::now()))
    }

    /// A feed whose clock a test drives by hand with
    /// [`advance`](Self::advance)/[`set_elapsed`](Self::set_elapsed), starting
    /// at t=0.
    ///
    /// Not `#[cfg(test)]`: `tests/*.rs` are separate crates, so a test-gated
    /// constructor would be invisible to the integration suite that needs it
    /// most.
    pub fn manual() -> Self {
        Self::with_clock(Cadence::Manual(Duration::ZERO))
    }

    fn with_clock(clock: Cadence) -> Self {
        Self {
            cursor: 0,
            next_index: 0,
            clock,
            echo: true,
            echoed: Vec::new(),
            last_resize: None,
            fed_chunks: 0,
            fed_bytes: 0,
        }
    }

    /// Turn the PTY-style echo (see the [module docs](self)) on or off.
    #[must_use]
    pub fn with_echo(mut self, echo: bool) -> Self {
        self.echo = echo;
        self
    }

    /// Move a [`manual`](Self::manual) clock forward by `delta`. No-op on a
    /// real clock.
    pub fn advance(&mut self, delta: Duration) {
        if let Cadence::Manual(elapsed) = &mut self.clock {
            *elapsed += delta;
        }
    }

    /// Place a [`manual`](Self::manual) clock at `elapsed` since the start of
    /// the replay. No-op on a real clock.
    pub fn set_elapsed(&mut self, elapsed: Duration) {
        if let Cadence::Manual(current) = &mut self.clock {
            *current = elapsed;
        }
    }

    /// Elapsed replay time — wall clock, or the manual cursor.
    pub fn elapsed(&self) -> Duration {
        self.clock.elapsed()
    }

    /// Chunks handed out so far.
    pub fn fed_chunks(&self) -> usize {
        self.fed_chunks
    }

    /// Fixture payload bytes handed out so far (echo excluded).
    pub fn fed_bytes(&self) -> usize {
        self.fed_bytes
    }

    /// Whether every fixture chunk has been handed out.
    pub fn is_drained(&self) -> bool {
        self.next_index >= CHUNK_COUNT
    }

    /// The last `(cols, rows)` [`TerminalFeed::resize`] was told about.
    pub fn last_resize(&self) -> Option<(u16, u16)> {
        self.last_resize
    }
}

impl Default for DemoFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalFeed for DemoFeed {
    /// Echo first, then **every** chunk whose scheduled time
    /// (`i * 1000 / HZ` ms) has passed.
    ///
    /// Echo leads because a PTY echoes a keystroke before the program under it
    /// answers — and it is one write however many keys arrived since the last
    /// wake, since the pump repaints once per wake either way.
    ///
    /// A late wake catches up on every due chunk in one pass rather than
    /// skipping any: the repaint is coalesced, the input never is.
    fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize {
        let mut writes = 0usize;
        if !self.echoed.is_empty() {
            sink(&self.echoed);
            self.echoed.clear();
            writes += 1;
        }

        let elapsed_ms = self.clock.elapsed().as_secs_f64() * 1000.0;
        while self.next_index < CHUNK_COUNT {
            if self.next_index as f64 * INTERVAL_MS > elapsed_ms {
                break;
            }
            match next_chunk(FIXTURE, &mut self.cursor) {
                Some(bytes) => {
                    sink(bytes);
                    self.fed_bytes += bytes.len();
                    self.fed_chunks += 1;
                    writes += 1;
                }
                // Truncated fixture: stop cleanly instead of spinning.
                None => {
                    self.next_index = CHUNK_COUNT;
                    break;
                }
            }
            self.next_index += 1;
        }
        writes
    }

    fn send_input(&mut self, bytes: &[u8]) {
        if self.echo {
            self.echoed.extend_from_slice(bytes);
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        // A replay has no window to resize — the fixture's own lines were
        // captured at FIXTURE_COLS x FIXTURE_ROWS and never re-flow. Recorded
        // so the caller's plumbing is testable before a session exists.
        self.last_resize = Some((cols, rows));
    }

    /// Open while the replay has chunks left, or while echo is on: an echoing
    /// feed stands in for a live PTY, which can answer a keystroke at any time.
    fn is_open(&self) -> bool {
        !self.is_drained() || self.echo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drain into a `Vec` of owned writes — the shape every test below asserts
    /// on.
    fn drain(feed: &mut DemoFeed) -> Vec<Vec<u8>> {
        let mut writes = Vec::new();
        let reported = feed.drain_output(&mut |bytes| writes.push(bytes.to_vec()));
        assert_eq!(
            reported,
            writes.len(),
            "the reported write count must match what the sink saw"
        );
        writes
    }

    #[test]
    fn the_embedded_fixture_is_the_checked_in_capture_byte_for_byte() {
        // A hand-rolled sha256 rather than a dependency: this is the one place
        // in the app that needs one, and adding a crate to the manifest for a
        // single test assertion is a worse trade than 40 lines of FIPS 180-4.
        assert_eq!(sha256_hex(FIXTURE), FIXTURE_SHA256);
    }

    #[test]
    fn the_fixture_walks_to_its_manifest_chunk_count_with_no_trailing_bytes() {
        let (count, offset) = walk_chunks(FIXTURE);
        assert_eq!(count, CHUNK_COUNT, "chunk count must match the manifest");
        assert_eq!(
            offset,
            FIXTURE.len(),
            "the whole file must be length-prefixed chunks (no trailing bytes)"
        );
    }

    #[test]
    fn chunk_reader_stops_on_a_truncated_tail_instead_of_panicking() {
        // A 3-byte payload declared, 2 supplied.
        let bytes = [3u8, 0, 0, 0, b'a', b'b'];
        let mut cursor = 0usize;
        assert!(next_chunk(&bytes, &mut cursor).is_none());
        assert_eq!(cursor, 0, "a rejected read must not advance the cursor");

        // A truncated 3-byte header.
        let short = [1u8, 0, 0];
        let mut cursor = 0usize;
        assert!(next_chunk(&short, &mut cursor).is_none());
    }

    #[test]
    fn replay_emits_chunk_i_at_i_over_hz_and_catches_up_on_a_late_wake() {
        let mut feed = DemoFeed::manual();

        // t=0 releases exactly chunk 0.
        assert_eq!(drain(&mut feed).len(), 1);
        // Just before chunk 1's slot (33.33ms): nothing new.
        feed.set_elapsed(Duration::from_micros(33_000));
        assert!(drain(&mut feed).is_empty());
        // Past it: exactly one more.
        feed.set_elapsed(Duration::from_micros(34_000));
        assert_eq!(drain(&mut feed).len(), 1);

        // A ~1-second-late wake must catch up on every due chunk (chunks
        // 0..=30 are due by t=1010ms), never skip one.
        feed.set_elapsed(Duration::from_millis(1010));
        assert_eq!(drain(&mut feed).len(), 29);
        assert_eq!(feed.fed_chunks(), 31);
    }

    #[test]
    fn the_whole_replay_drains_to_its_chunk_count_and_payload_length() {
        let mut feed = DemoFeed::manual().with_echo(false);
        feed.set_elapsed(Duration::from_secs(60));
        drain(&mut feed);
        assert!(feed.is_drained());
        assert_eq!(feed.fed_chunks(), CHUNK_COUNT);
        // Payload bytes = file length minus one 4-byte header per chunk.
        assert_eq!(feed.fed_bytes(), FIXTURE.len() - 4 * CHUNK_COUNT);
        assert!(
            !feed.is_open(),
            "an echo-less replay closes once it has drained — the pump's exit"
        );
    }

    #[test]
    fn an_echoing_feed_stays_open_past_the_end_of_the_replay() {
        let mut feed = DemoFeed::manual();
        feed.set_elapsed(Duration::from_secs(60));
        drain(&mut feed);
        assert!(feed.is_drained());
        assert!(
            feed.is_open(),
            "a PTY stand-in can still answer a keystroke"
        );
    }

    #[test]
    fn echo_round_trips_sent_bytes_into_the_output_stream() {
        let mut feed = DemoFeed::manual();
        // Drain chunk 0 out of the way first, so the echo write is unambiguous.
        drain(&mut feed);

        feed.send_input(b"ls");
        feed.send_input(b" -la\r");
        let writes = drain(&mut feed);
        assert_eq!(
            writes.first().map(Vec::as_slice),
            Some(b"ls -la\r".as_slice()),
            "everything sent since the last wake echoes as ONE write, in order"
        );
        // …and only once.
        let writes = drain(&mut feed);
        assert!(writes.iter().all(|w| w != b"ls -la\r"));
    }

    #[test]
    fn echo_off_swallows_sent_bytes() {
        let mut feed = DemoFeed::manual().with_echo(false);
        drain(&mut feed);
        feed.send_input(b"ls\r");
        assert!(drain(&mut feed).is_empty());
    }

    #[test]
    fn resize_is_recorded_for_the_session_that_will_one_day_care() {
        let mut feed = DemoFeed::manual();
        assert_eq!(feed.last_resize(), None);
        feed.resize(80, 24);
        assert_eq!(feed.last_resize(), Some((80, 24)));
        feed.resize(52, 30);
        assert_eq!(feed.last_resize(), Some((52, 30)));
    }

    #[test]
    fn the_first_chunk_is_the_build_log_line_the_grid_paints_first() {
        let mut feed = DemoFeed::manual();
        let writes = drain(&mut feed);
        let first = String::from_utf8_lossy(&writes[0]).to_string();
        assert!(first.contains("INFO"), "{first:?}");
        assert!(first.contains("[00000]"), "{first:?}");
    }

    // ---------------------------------------------------------------
    // A minimal sha256 (FIPS 180-4) for the fixture-integrity test above
    // ---------------------------------------------------------------

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    fn sha256_hex(data: &[u8]) -> String {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let mut message = data.to_vec();
        let bit_len = (data.len() as u64) * 8;
        message.push(0x80);
        while message.len() % 64 != 56 {
            message.push(0);
        }
        message.extend_from_slice(&bit_len.to_be_bytes());

        for block in message.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, word) in block.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes(word.try_into().expect("4-byte word"));
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *slot = slot.wrapping_add(value);
            }
        }

        h.iter().map(|word| format!("{word:08x}")).collect()
    }
}
