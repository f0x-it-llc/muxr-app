//! The keystroke source's **pure half**: the last-known-state diff that turns
//! platform IME snapshots back into terminal bytes, the one-shot Ctrl/Alt
//! latch every emission passes through, and the key bar's own byte vocabulary.
//!
//! No `Widget`, no `Rc`, no signals, no feed — everything here is a plain
//! value transformation, so the whole input model is unit-testable without a
//! render tree. [`widget`](super::widget) owns the IME clienthood
//! (`request_focus`/`publish_ime_state`) and
//! [`keybar`](crate::features::terminal::presentation::keybar) owns the bar's
//! chrome; both drive [`KeystrokeSource`] and send whatever bytes it hands back
//! to [`TerminalFeed::send_input`](crate::features::terminal::domain::feed::TerminalFeed::send_input).
//!
//! | Module | Owns |
//! |---|---|
//! | `bytes` | the escape/sequence vocabulary, [`BarKey`], the named-key table |
//! | `latch` | [`ModifierLatch`] and the Ctrl/Alt transform |
//! | `utf16` | the seam's index arithmetic and the mirror [`diff`] |
//! | `source` | [`KeystrokeSource`]: the diff engine, its rolling window, its desync guards |
//!
//! # Why a diff, and not a sentinel buffer
//!
//! Both mobile IME bridges are **state-sync, never op-forwarding** (frust's
//! `docs/CODE_STANDARDS.md`): the platform owns a text mirror and hands the app
//! a whole reconciled [`EditingState`](frust::authoring::EditingState), never a
//! keystroke. A terminal needs the opposite — one byte per key. Recovering the
//! keystroke by priming that mirror with a fixed invisible **sentinel** and
//! diffing every snapshot against it carries the failure this module is built
//! to make impossible: an iOS smart-dash rewrite can rewrite the *prefix*, and
//! the sentinel characters then leak into the byte stream as if the user had
//! typed them.
//!
//! So there is **no sentinel here, and no invented character anywhere**. The
//! source keeps the last snapshot it reconciled and diffs the next one against
//! it in UTF-16 space:
//!
//! - appended text ⇒ those code units, as UTF-8 bytes;
//! - a shortened buffer ⇒ one [`BACKSPACE_BYTE`] per **removed UTF-16 code
//!   unit** (an emoji is two units and therefore two backspaces — the platform
//!   reports units, so units is what a delete costs);
//! - a replaced span ⇒ the backspaces, then the bytes.
//!
//! Every byte the source ever emits is therefore a **substring of something the
//! platform reported**. There is no character in the mirror the user did not
//! type, so no rewrite of it can invent input.
//!
//! # Rules, each pinned by a test
//!
//! - **Zero-composing is the norm.** A swipe-typed word arrives as ONE
//!   multi-character commit ⇒ one contiguous byte burst. If a composing range
//!   *does* appear it is **report-only**: nothing is emitted and the window is
//!   not moved, because re-publishing mid-composition destroys the platform's
//!   own composition state. [`KeystrokeSource::published`] therefore echoes the
//!   composing snapshot verbatim, so the paint-pass republish cannot fight the
//!   composition either.
//! - **Enter is asymmetric.** Android delivers `Key::Named(Enter)` (its
//!   `IME_ACTION_DONE`); iOS delivers a literal `"\n"` inside the snapshot.
//!   Both emit [`ENTER_BYTE`] (`0x0d`, CR) — what a terminal expects for
//!   Return. Every `\n` inside a committed run is translated the same way, so a
//!   pasted multi-line block is several Returns.
//! - **Backspace is deduped.** Android can deliver *both* a
//!   `Key::Named(Backspace)` and the matching snapshot shortening: the key path
//!   emits and arms, the reconciler consumes the echo, so one `0x7f` reaches the
//!   session and not two. A snapshot-only shortening (iOS, and Gboard's
//!   `deleteSurroundingText`) still emits exactly one. The arm is retired by the
//!   first snapshot that does *not* shorten, so a key event whose echo never
//!   arrives cannot swallow a later real delete.
//! - **Repeated identical characters each emit.** `"aaa"` is three appends of
//!   one unit, three bytes.
//! - **Nothing is transformed.** [`ImeContentType::Terminal`](frust::authoring::ImeContentType)
//!   suppresses smart quotes/dashes platform-side; this module additionally
//!   never rewrites a byte it was handed. `"` is `0x22` and `--` is `2d 2d`.
//!
//! # What is retained
//!
//! Everything in the mirror and the history is, by construction, **text the
//! user typed** — which on a terminal includes the `sudo` password, the ssh
//! passphrase and the pasted API token. Remote echo-off does not help: the IME
//! reports the keystroke to the app either way, because the app *is* the
//! keyboard's client. So every buffer here is a
//! [`Zeroizing<String>`](zeroize::Zeroizing) rewritten in place rather than
//! reallocated, [`KeystrokeSource::scrub`] drops all of them at every boundary
//! where the text stops being live, and the one thing a scrub keeps about what
//! it dropped is a keyed digest — never a character of it. The buffers, the
//! bounds that keep a paste from growing them, and the residuals this leaves
//! are documented on [`KeystrokeSource`] itself.

mod bytes;
mod latch;
mod source;
mod utf16;

#[cfg(test)]
mod harness;

pub use bytes::{
    ARROW_DOWN, ARROW_LEFT, ARROW_RIGHT, ARROW_UP, BACKSPACE_BYTE, BarKey, END, ENTER_BYTE, ESC,
    FORWARD_DELETE, HOME, PAGE_DOWN, PAGE_UP, TAB, named_key_bytes, text_bytes,
};
pub use latch::{ModifierLatch, apply_latch, single_unit};
pub use source::{
    Emission, HISTORY_CAP, InputClass, KeystrokeSource, MAX_PENDING_BACKSPACES, MAX_POST_SCRUB_ADD,
    MAX_SNAPSHOT_DELETE, MIRROR_CAP, MIRROR_FLOOR, MIRROR_TAIL,
};
pub use utf16::{diff, len_utf16, utf16_suffix};
