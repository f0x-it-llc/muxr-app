//! The keystroke source's **pure half**: the last-known-state diff that turns
//! platform IME snapshots back into terminal bytes, the one-shot Ctrl/Alt/Shift
//! latch every emission passes through, and the key bar's own byte vocabulary. No
//! `Widget`, no `Rc`, no signals, no feed — everything here is a plain value
//! transformation, so the whole input model is unit-testable without a render
//! tree. `bytes` owns the escape/sequence vocabulary and [`BarKey`], `latch` the
//! [`ModifierLatch`], `utf16` the index arithmetic and the mirror [`diff`], and
//! `source` the diff engine, its rolling window and its desync guards.
//!
//! **Why a diff, and not a sentinel buffer.** Both mobile IME bridges are
//! **state-sync, never op-forwarding** (frust's `docs/CODE_STANDARDS.md`): the
//! platform owns a text mirror and hands the app a whole reconciled
//! [`EditingState`](frust::authoring::EditingState), never a keystroke, while a
//! terminal needs one byte per key. Priming that mirror with a fixed invisible
//! **sentinel** carries the failure this module is built to make impossible: an
//! iOS smart-dash rewrite can rewrite the *prefix*, and the sentinel characters
//! then leak into the byte stream as if typed. So there is **no sentinel here, and
//! no invented character anywhere** — the source diffs each snapshot against the
//! last it reconciled, in UTF-16 space, so every byte it emits is a **substring of
//! something the platform reported**. [`KeystrokeSource`] documents the window,
//! the guards and the buffer-retention contract; the asymmetries (Android's Return
//! as a key event, its backspace echo, `report-only` compositions) are stated on
//! the methods that implement them, each pinned by a test.
//!
//! **What is retained** is, by construction, text the user typed — which on a
//! terminal includes the `sudo` password and the pasted API token, since the app
//! *is* the keyboard's client. Every buffer is a
//! [`Zeroizing<String>`](zeroize::Zeroizing) rewritten in place rather than
//! reallocated, and [`KeystrokeSource::scrub`] drops all of them at every boundary
//! where the text stops being live.

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
