//! Terminal keystroke input: diffs platform IME snapshots back into terminal bytes.
//! Every emitted byte is a substring of text the platform reported; no sentinel is primed.
//! Buffers are `Zeroizing`; `KeystrokeSource::scrub` drops them when text stops being live.

mod bytes;
mod latch;
mod source;
mod utf16;

#[cfg(test)]
mod harness;

pub use bytes::{
    ARROW_DOWN, ARROW_LEFT, ARROW_RIGHT, ARROW_UP, BACK_TAB, BACKSPACE_BYTE, BarKey, END,
    ENTER_BYTE, ESC, FORWARD_DELETE, HOME, PAGE_DOWN, PAGE_UP, SHIFT_ARROW_DOWN, SHIFT_ARROW_LEFT,
    SHIFT_ARROW_RIGHT, SHIFT_ARROW_UP, SHIFT_FORWARD_DELETE, SHIFT_PAGE_DOWN, SHIFT_PAGE_UP, TAB,
    named_key_bytes, shifted_sequence, text_bytes,
};
pub use latch::{ModifierLatch, apply_latch, single_unit};
pub use source::{
    Emission, HISTORY_CAP, InputClass, KeystrokeSource, MAX_PENDING_BACKSPACES, MAX_POST_SCRUB_ADD,
    MAX_SNAPSHOT_DELETE, MIRROR_CAP, MIRROR_FLOOR, MIRROR_TAIL,
};
pub use utf16::{diff, len_utf16, utf16_suffix};
