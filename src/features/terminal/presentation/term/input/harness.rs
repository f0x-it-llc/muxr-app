//! Fixtures shared by this module's unit tests: the snapshot shape a platform
//! mirror delivers, hex formatting for byte assertions, and the per-keystroke
//! typing loop.

use frust::authoring::EditingState;

use super::source::KeystrokeSource;
use super::utf16::len_utf16;

/// The ordinary mobile snapshot shape: caret collapsed at the end, no
/// composing region.
pub(super) fn snapshot(text: &str) -> EditingState {
    let end = i32::try_from(len_utf16(text)).unwrap_or(i32::MAX);
    EditingState {
        text: text.to_string(),
        selection_base: end,
        selection_extent: end,
        composing_base: -1,
        composing_extent: -1,
    }
}

/// Lowercase hex pairs — how every byte assertion reads.
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Type `text` one character at a time, exactly as a platform mirror would:
/// each snapshot is **the mirror the source currently publishes** plus one
/// character.
///
/// Re-reading the mirror per keystroke is not a shortcut — it is the contract.
/// Both bridges apply a re-anchor synchronously inside the same round trip that
/// delivered the keystroke (see [`source`](super::source)'s docs), so the
/// snapshot after a trim is built on the *trimmed* buffer.
pub(super) fn type_text(source: &mut KeystrokeSource, text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in text.chars() {
        let mut buffer = source.mirror().to_string();
        buffer.push(ch);
        out.extend(source.on_snapshot(&snapshot(&buffer)).bytes);
    }
    out
}
