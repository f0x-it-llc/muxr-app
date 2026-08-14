//! The engine: [`KeystrokeSource`], the rolling mirror window it diffs against,
//! and the guards that tell typing apart from a bridge that has fallen out of
//! step. The [module docs](super) hold the diff contract this implements.
//!
//! **The mirror is a rolling window, re-anchored only on demand** — publishing per
//! keystroke would be a platform `restartInput` per keystroke. It re-anchors to
//! the last [`MIRROR_TAIL`] units when it grows past [`MIRROR_CAP`] or shrinks
//! below [`MIRROR_FLOOR`] (refilling from [`HISTORY_CAP`] units of history), and
//! never to *empty*: a deletion is only observable as a **shortening**, so a tail
//! of the user's own recent typing is what keeps held-down backspace emitting.
//! Sound only because **both bridges apply the republished state synchronously,
//! inside the round trip that delivered the keystroke** — the one premise here
//! that lives outside this app.
//!
//! **A submitted line is dropped, not kept as a baseline**: every emission
//! carrying a Return re-anchors to empty, mirror and history both, so no `\n`
//! survives to be bulk re-emitted into the remote host. But **the platform may
//! still hold that line for a few frames** (a submit is not applied synchronously
//! the way an edit is), and diffed against the empty baseline it reads as one
//! `added` run carrying the whole line — which, emitted, re-executes a command or
//! re-transmits a password. So every scrub arms a **one shot**
//! ([`KeystrokeSource::scrub`]), whose two tests
//! ([`KeystrokeSource::is_replay`], [`is_one_commit`]), narrowings and accepted
//! trades are documented where they are enforced. An **oversized single edit is a
//! desync, not intent**: a `removed` above [`MAX_SNAPSHOT_DELETE`] emits nothing
//! and re-publishes the mirror the source still believes in.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

use frust::authoring::{EditingState, Key, Modifiers, NamedKey};
use zeroize::{Zeroize, Zeroizing};

use super::bytes::{BACKSPACE_BYTE, BarKey, named_key_bytes, shifted_sequence, text_bytes};
use super::latch::{ModifierLatch, apply_latch, single_unit};
use super::utf16::{clamp_front, clamp_index, diff, len_utf16, utf16_prefix, utf16_suffix};

// The rolling window.

/// How many UTF-16 code units the published mirror may reach before a re-anchor.
pub const MIRROR_CAP: usize = 96;

/// The most units a snapshot may ever be **stored** as, and the reason
/// `set_mirror` clamps: a pasted token arrives far longer than the cap, and
/// storing it whole would grow the buffer past the capacity
/// [`KeystrokeSource::new`] pre-reserved — a `String` reallocation frees plaintext
/// no scrub can reach. **One past the cap, not at it**: `reanchor_if_needed` fires
/// on `len > MIRROR_CAP`, so clamping to the cap would disable the re-anchor.
const MIRROR_STORE_CAP: usize = MIRROR_CAP + 1;

/// How many units a re-anchor leaves in the mirror. The gap to [`MIRROR_CAP`] is
/// how many characters may be typed between two platform `restartInput`s.
pub const MIRROR_TAIL: usize = 24;

/// How short the mirror may get before a re-anchor refills it from history —
/// never zero, because a delete is only observable as a *shortening*.
pub const MIRROR_FLOOR: usize = 4;

/// How many units of the user's own recent typing the source retains to refill
/// the mirror from. Past it, the oldest typing can no longer be backspaced into.
pub const HISTORY_CAP: usize = 256;

/// How many un-echoed `Key::Named(Backspace)`s may be armed at once — the dedupe
/// rule [`on_key`](KeystrokeSource::on_key) arms and
/// [`on_snapshot`](KeystrokeSource::on_snapshot) consumes. A ceiling, not a queue.
pub const MAX_PENDING_BACKSPACES: usize = 4;

/// How many UTF-16 code units one snapshot may delete before the source reads it
/// as a **desync** rather than typing, emits nothing and re-publishes.
///
/// **8**, derived: both bridges deliver a snapshot per IME round trip, a held
/// Backspace repeats at the platform key-repeat rate, and a deleted surrogate pair
/// costs two units — so two or three collapsed deletes (≤6) is the ceiling for
/// genuine input, and 8 keeps headroom while staying far below [`MIRROR_TAIL`].
/// The trade: a genuine longer delete run is suppressed and the mirror restored.
pub const MAX_SNAPSHOT_DELETE: usize = 8;

/// How many UTF-16 code units **one keystroke** can be, and so the shortest add
/// the post-scrub guard admits without looking at its shape ([`is_one_commit`]).
/// **2**, because an emoji panel commits a surrogate pair for one tap.
const SINGLE_KEYSTROKE_UNITS: usize = 2;

/// The longest single IME **commit** the post-scrub guard admits as genuine first
/// input ([`is_one_commit`]).
///
/// **16**, derived: the only multi-unit add a soft keyboard produces in one go is
/// a gesture-typed word, which frust's Android bridge deliberately keeps working
/// on an [`ImeContentType::Terminal`](frust::authoring::ImeContentType) field —
/// fifteen letters plus a gesture commit's trailing space. Not smaller because a
/// bound tight enough to reject every short stale line would reject those commits
/// too; a word-shaped stale line is caught by the **content** half of the guard.
pub const MAX_POST_SCRUB_ADD: usize = 16;

/// The shortest dropped baseline the post-scrub guard records a replay digest for
/// ([`KeystrokeSource::scrub`]). **One unit past [`SINGLE_KEYSTROKE_UNITS`]**:
/// below it the content test buys nothing and costs a key, since such a replay is
/// one keystroke of bytes that no rule can tell from the same key pressed again,
/// while refusing it would make that key **permanently** untypeable.
const MIN_REPLAY_DIGEST_UNITS: usize = SINGLE_KEYSTROKE_UNITS + 1;

// Snapshot predicates.

/// `state`'s composing range, **validated** against its own text, or `None` when
/// it is not composing. The platform's indices are taken on trust nowhere else in
/// this module, and this is the one place they are echoed straight back out — so a
/// base past the end, an inverted range or a half-set pair resolves to "not
/// composing" rather than being handed back as a range into text that cannot hold
/// it.
fn composing_range(state: &EditingState) -> Option<(i32, i32)> {
    if state.composing_base < 0 || state.composing_extent <= state.composing_base {
        return None;
    }
    let len = i32::try_from(len_utf16(&state.text)).unwrap_or(i32::MAX);
    let base = state.composing_base.min(len);
    let extent = state.composing_extent.min(len);
    (extent > base).then_some((base, extent))
}

/// Whether `text` carries a Return — the trigger for dropping the baseline. Both
/// spellings, because a pasted `"\r\n"` is two Returns on the way out.
fn carries_return(text: &str) -> bool {
    text.contains(['\n', '\r'])
}

/// Whether `text` is a Return and **nothing else** — the one Return-carrying add
/// the stale-snapshot guard lets through: it has no command text to replay.
fn is_bare_return(text: &str) -> bool {
    matches!(text, "\n" | "\r" | "\r\n")
}

/// Whether `added` is at most **one commit's worth** of input — the post-scrub
/// replay guard's shape test, in three rules:
///
/// 1. **A Return is admitted only bare.** A Return *with* text is the submitted
///    line and its newline, which `text_bytes` maps to CR and re-executes.
/// 2. **Any single keystroke passes** ([`SINGLE_KEYSTROKE_UNITS`]).
/// 3. **Longer, and it has to look like one word**: at most
///    [`MAX_POST_SCRUB_ADD`] units of alphanumerics plus the one trailing space a
///    gesture commit appends. `"status "` is one commit; `"sudo -i"` is not.
///
/// A **shape** test, not a proof, and deliberately only half the guard: a stale
/// line that is itself one bare word is refused by the **content** test the caller
/// runs first ([`KeystrokeSource::is_replay`]).
fn is_one_commit(added: &str) -> bool {
    if carries_return(added) {
        return is_bare_return(added);
    }
    let units = len_utf16(added);
    if units <= SINGLE_KEYSTROKE_UNITS {
        return true;
    }
    if units > MAX_POST_SCRUB_ADD {
        return false;
    }
    added
        .strip_suffix(' ')
        .unwrap_or(added)
        .chars()
        .all(char::is_alphanumeric)
}

// What one input event derived.

/// What the source concluded about one event — diagnostics, and the classification
/// the tests assert on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputClass {
    /// Nothing derivable: an unchanged snapshot (our own echo), a composition
    /// bracket, a latch toggle's own tap.
    #[default]
    None,
    /// Committed text.
    Text,
    /// One or more deletions.
    Backspace,
    /// Return, from either platform's path.
    Enter,
    /// A live composing region — reported, never committed.
    Composing,
    /// A key-bar or named-key control sequence.
    Sequence,
    /// A latch key was toggled.
    Latch,
    /// A snapshot read as a bridge/dictation **desync** rather than as typing: one
    /// deleting more than [`MAX_SNAPSHOT_DELETE`] units, or — into the empty
    /// baseline a scrub left — a just-submitted line replayed. Nothing is emitted,
    /// the mirror is re-published, and the guard stays armed for the next frame.
    Desync,
}

/// One event's outcome: the bytes to send, and whether to re-publish the mirror.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Emission {
    /// The bytes to hand to
    /// [`TerminalFeed::send_input`](crate::features::terminal::domain::feed::TerminalFeed::send_input).
    pub bytes: Vec<u8>,
    /// The source re-anchored its window: the caller must publish
    /// [`KeystrokeSource::published`] so the platform mirror follows.
    pub reconciled: bool,
    /// What this event was.
    pub class: InputClass,
}

impl Emission {
    /// An outcome that derived nothing.
    fn inert(class: InputClass) -> Self {
        Self {
            bytes: Vec::new(),
            reconciled: false,
            class,
        }
    }

    /// Whether anything at all happened — what the caller gates its wake on.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty() && !self.reconciled && self.class == InputClass::None
    }
}

// The source itself.

/// The live composing snapshot, held the way every other buffer here is: the
/// platform's text in a [`Zeroizing`] `String` plus the **already validated**
/// indices that arrived with it. Not an [`EditingState`]: a composing snapshot
/// carries the whole mobile mirror, a password mid-composition included, so
/// parking it as one would leave an unscrubbed plaintext copy per keystroke.
struct ComposingEcho {
    /// The platform's text, verbatim. Never clamped the way the mirror is: a
    /// composition's indices are indices into *this*.
    text: Zeroizing<String>,
    /// The caret/selection, clamped into [`text`](Self::text).
    selection_base: i32,
    /// The selection's other end, clamped the same way.
    selection_extent: i32,
    /// The composing range's start, validated against [`text`](Self::text).
    composing_base: i32,
    /// The composing range's end, validated the same way.
    composing_extent: i32,
}

impl ComposingEcho {
    /// An empty echo, pre-allocated at the same bound the mirror is (see
    /// [`KeystrokeSource::new`]) so an ordinary composition never grows it.
    fn new() -> Self {
        Self {
            text: Zeroizing::new(String::with_capacity(MIRROR_STORE_CAP * 3)),
            selection_base: -1,
            selection_extent: -1,
            composing_base: -1,
            composing_extent: -1,
        }
    }

    /// The state to publish — rebuilt per call rather than stored, so the plain
    /// copy lives only as long as the platform's own round trip.
    fn state(&self) -> EditingState {
        EditingState {
            text: self.text.as_str().to_owned(),
            selection_base: self.selection_base,
            selection_extent: self.selection_extent,
            composing_base: self.composing_base,
            composing_extent: self.composing_extent,
        }
    }

    /// How many UTF-16 units of the user's own text this is holding.
    fn units(&self) -> usize {
        len_utf16(&self.text)
    }
}

/// The keystroke source: the platform-mirror baseline, the modifier latch, and the
/// counters the tests read. One instance is shared (behind an `Rc<RefCell<_>>`)
/// between the grid widget and the key bar, so an emission from either passes
/// through the same latch.
///
/// **Every buffer below holds the user's own typing, secrets included**: each is
/// [`Zeroizing`], each is rewritten in place, and nothing oversized is ever
/// *stored* — [`set_mirror`](Self::set_mirror) keeps only the clamped suffix, so a
/// pasted token cannot grow either diff buffer past the capacity
/// [`new`](Self::new) pre-reserved. The **composing echo is deliberately
/// unbounded**, since a composition's indices are indices into it — safe because
/// it too is only ever written `zeroize()`-then-`push_str`.
pub struct KeystrokeSource {
    /// What the platform mirror is believed to hold — the diff baseline. Always
    /// a suffix of [`history`](Self::history).
    mirror: Zeroizing<String>,
    /// The user's own recent typing, [`HISTORY_CAP`] units of it. A re-anchor
    /// refills the mirror from here; nothing else reads it.
    history: Zeroizing<String>,
    /// The live composing snapshot, while one exists — echoed verbatim by
    /// [`published`](Self::published) so the paint-pass republish cannot destroy
    /// the platform's composition.
    composing: Option<ComposingEcho>,
    /// **One shot, armed by every [`scrub`](Self::scrub)**: the baseline was just
    /// dropped to empty, so the platform may still hold the submitted line. Spent
    /// by the next snapshot that actually **adds** something *and is let through*,
    /// and by nothing else; [`desync`](Self::desync) re-arms it.
    baseline_dropped: bool,
    /// A **keyed digest and UTF-16 length** of the baseline the last
    /// [`scrub`](Self::scrub) dropped — the content half of the post-scrub guard,
    /// and the only thing about a dropped line that survives it. **No plaintext.**
    /// `None` when the dropped baseline was at most one keystroke's worth
    /// ([`MIN_REPLAY_DIGEST_UNITS`]) and when the shot has been spent.
    dropped: Option<(u64, usize)>,
    /// This instance's own hash seed, so [`dropped`](Self::dropped)'s digest is
    /// meaningless anywhere else: not comparable between two sources or processes,
    /// and says nothing about the line without the in-process key.
    replay_seed: RandomState,
    /// Armed by a `Key::Named(Backspace)`, consumed by the matching snapshot
    /// shortening (the dedupe rule).
    pending_backspaces: usize,
    /// The one-shot Ctrl/Alt/Shift latch.
    latch: ModifierLatch,
    /// Bumped on every state change — mirrored into the seam's signal so the key
    /// bar's armed pips rebuild.
    revision: u64,
    /// How many times the window has been re-anchored (and therefore how many
    /// times the platform mirror had to be re-published).
    reconciles: u64,
    /// Total bytes ever emitted — a diagnostic/test accessor.
    emitted: usize,
}

impl Default for KeystrokeSource {
    fn default() -> Self {
        Self::new()
    }
}

impl KeystrokeSource {
    /// A source with an empty mirror and nothing armed.
    ///
    /// Both buffers are allocated at their steady-state capacity up front: a
    /// `String` that grows reallocates, and a reallocation frees the old plaintext
    /// where no scrub can reach it. Three bytes per UTF-16 unit is an upper bound
    /// for any text at all. The history reserves **one unit more** than
    /// [`HISTORY_CAP`], because `clamp_front` rounds *outward* ([`utf16_suffix`]):
    /// a clamp landing mid-surrogate-pair legitimately leaves `HISTORY_CAP + 1`
    /// units, one reallocation past a `HISTORY_CAP * 3` reserve. That matters here
    /// and nowhere else — `set_mirror` **appends** to the live history, where the
    /// mirror and echo are rewritten zeroize-then-push and can only free zeros.
    pub fn new() -> Self {
        Self {
            mirror: Zeroizing::new(String::with_capacity(MIRROR_STORE_CAP * 3)),
            history: Zeroizing::new(String::with_capacity((HISTORY_CAP + 1) * 3)),
            composing: None,
            baseline_dropped: false,
            dropped: None,
            replay_seed: RandomState::new(),
            pending_backspaces: 0,
            latch: ModifierLatch::NONE,
            revision: 0,
            reconciles: 0,
            emitted: 0,
        }
    }

    /// The editing state to publish right now: the mirror with the caret collapsed
    /// at its end — **except** while a composition is live, where it is the
    /// platform's own snapshot echoed back, so re-publishing it is a no-op on both
    /// bridges instead of a composition-destroying wholesale replace.
    pub fn published(&self) -> EditingState {
        if let Some(echo) = &self.composing {
            return echo.state();
        }
        let end = i32::try_from(len_utf16(&self.mirror)).unwrap_or(i32::MAX);
        EditingState {
            text: self.mirror.as_str().to_owned(),
            selection_base: end,
            selection_extent: end,
            composing_base: -1,
            composing_extent: -1,
        }
    }

    /// The armed modifiers (the key bar's pips read this).
    pub fn latch(&self) -> ModifierLatch {
        self.latch
    }

    /// The mirror's current text — a **test-only** accessor, and the one thing here
    /// that hands out the user's own plaintext, so it is compiled out of every
    /// non-test build. Production reads it through [`published`](Self::published).
    #[cfg(test)]
    pub fn mirror(&self) -> &str {
        &self.mirror
    }

    /// How many UTF-16 code units of the user's own typing the source is holding —
    /// the **content-free** counterpart of [`mirror`](Self::mirror), and what a
    /// scrub boundary is asserted on (`0` is the only value that means "nothing
    /// retained"). An upper bound rather than an exact count: the mirror is a
    /// suffix of the history by invariant, so a non-zero answer counts those units
    /// twice — the safe direction for the question this exists to answer.
    pub fn buffered_units(&self) -> usize {
        len_utf16(&self.mirror)
            + len_utf16(&self.history)
            + self.composing.as_ref().map_or(0, ComposingEcho::units)
    }

    /// How many times the window has been re-anchored (and therefore published).
    pub fn reconciles(&self) -> u64 {
        self.reconciles
    }

    /// Bumped on every state change; mirrored into the seam's wake signal.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Total bytes emitted so far — a diagnostic/test accessor.
    pub fn emitted(&self) -> usize {
        self.emitted
    }

    /// Whether a composition is live (report-only — see `on_snapshot`'s step 1).
    pub fn is_composing(&self) -> bool {
        self.composing.is_some()
    }

    // -- the scrub -----------------------------------------------------------

    /// **Drop everything the user typed**: both buffers, the composing echo, and
    /// the backspace arm that only means anything against them.
    ///
    /// Zeroing rather than clearing — `zeroize` writes across each buffer's whole
    /// capacity, so the stale tail an in-place truncation leaves behind goes too;
    /// capacity survives, so a scrub never reallocates. After this the diff
    /// baseline is **empty**, which is right at every boundary this is called from
    /// (a submitted line, a blur, `read_only`, teardown). The **latch** is
    /// deliberately left alone: an armed `Ctrl` is not a secret, and clearing it
    /// would owe the key bar a signal write from paths (a paint-pass blur) that
    /// must not make one.
    ///
    /// Dropping the baseline also arms [`baseline_dropped`](Self::baseline_dropped)
    /// and records [`dropped`](Self::dropped), that shot's content test.
    pub fn scrub(&mut self) {
        let units = len_utf16(&self.mirror);
        self.dropped = if units >= MIN_REPLAY_DIGEST_UNITS {
            Some((self.digest(&self.mirror), units))
        } else {
            None
        };
        self.mirror.zeroize();
        self.history.zeroize();
        // `None` drops the echo, and its `Zeroizing` text scrubs on that drop.
        self.composing = None;
        self.pending_backspaces = 0;
        self.baseline_dropped = true;
        self.revision += 1;
    }

    /// A line has been submitted: drop the baseline and report that the caller must
    /// re-publish. Counted as a re-anchor because that is what it costs — one
    /// platform `restartInput`, on the rarest keystroke there is.
    fn submit_line(&mut self) -> bool {
        self.scrub();
        self.reconciles += 1;
        true
    }

    /// The answer to a snapshot the source reads as a **desync** rather than as
    /// typing: emit nothing, move nothing, and re-publish the state the source
    /// still believes in — which is what pulls the platform back into step. Both
    /// guards end here. The backspace arm goes too — the echo it was waiting for is
    /// not in a snapshot like this one.
    ///
    /// **Refusing a snapshot re-arms the one shot.** This is the one place that
    /// both refuses a snapshot *and* provably leaves the baseline it was diffed
    /// against exactly as it was, so the guard's precondition still holds on
    /// return. It cannot livelock: emitting nothing, moving neither buffer and
    /// re-publishing what was already believed is idempotent.
    fn desync(&mut self) -> Emission {
        self.pending_backspaces = 0;
        self.baseline_dropped = true;
        self.reconciles += 1;
        self.revision += 1;
        Emission {
            bytes: Vec::new(),
            reconciled: true,
            class: InputClass::Desync,
        }
    }

    /// A keyed digest of `text` under this instance's own seed — the only form a
    /// dropped baseline is ever kept in.
    fn digest(&self, text: &str) -> u64 {
        self.replay_seed.hash_one(text)
    }

    /// Whether `added` **begins with the baseline the last scrub dropped** — the
    /// content half of the post-scrub guard. A *prefix* rather than the whole add,
    /// so the stale line plus whatever the user typed during the window
    /// (`"hunter2x"`, iOS's `"hunter2\n"`) is caught as well as the exact replay.
    /// The length check makes the comparison sound: [`utf16_prefix`] rounds inward,
    /// so an add too short to hold the baseline comes up short and is not a match.
    fn is_replay(&self, added: &str) -> bool {
        let Some((digest, units)) = self.dropped else {
            return false;
        };
        let head = utf16_prefix(added, units);
        len_utf16(head) == units && self.digest(head) == digest
    }

    /// Record one composing snapshot, rewriting the echo's buffer **in place** so a
    /// composing keystroke leaves no unscrubbed plaintext copy behind.
    /// `selection`/`composing` are `(base, extent)` pairs already validated.
    fn set_composing(&mut self, text: &str, selection: (i32, i32), composing: (i32, i32)) {
        let echo = self.composing.get_or_insert_with(ComposingEcho::new);
        echo.text.zeroize();
        echo.text.push_str(text);
        echo.selection_base = selection.0;
        echo.selection_extent = selection.1;
        echo.composing_base = composing.0;
        echo.composing_extent = composing.1;
    }

    // -- the emission choke point -------------------------------------------

    /// **The one place bytes are produced.** Applies the latch (plus any transient
    /// hardware modifiers) and, on a single-code-unit emission, disarms *both*
    /// flags together. A longer emission — an arrow sequence, a swipe-typed word, a
    /// run of backspaces — passes through untransformed and leaves the latch armed.
    fn emit(&mut self, data: &[u8], transient: ModifierLatch) -> Vec<u8> {
        if data.is_empty() {
            return Vec::new();
        }
        let effective = self.latch.union(transient);
        let out = apply_latch(effective, data);
        if single_unit(data).is_some() && self.latch.armed() {
            self.latch = ModifierLatch::NONE;
        }
        self.emitted += out.len();
        self.revision += 1;
        out
    }

    /// Emit one **sequence** key — a bar cap's or a hardware [`NamedKey`]'s —
    /// choosing its Shift variant where an armed `Shift` has one
    /// ([`shifted_sequence`]). A separate step from [`emit`](Self::emit)'s
    /// transform because that transform only ever rewrites a **single code unit**,
    /// where most of these are longer — and even `Tab` answers Shift with a whole
    /// other sequence rather than a modified byte. Choosing one consumes the
    /// latch's `shift`; `Ctrl`/`Alt` are deliberately *not* consumed here and do
    /// not reach the bytes at all (see [`ModifierLatch`]).
    fn emit_sequence(&mut self, sequence: &'static [u8], transient: ModifierLatch) -> Vec<u8> {
        let shifted = (self.latch.shift || transient.shift)
            .then(|| shifted_sequence(sequence))
            .flatten();
        let out = self.emit(shifted.unwrap_or(sequence), transient);
        if shifted.is_some() {
            self.latch.shift = false;
        }
        out
    }

    // -- the mirror window ---------------------------------------------------

    /// Accept `next` as the platform mirror, folding it into history (whose tail
    /// *is* the old mirror, by invariant).
    ///
    /// Takes a `&str` and rewrites both buffers **in place** rather than taking a
    /// `String` it could park: a per-keystroke `state.text.clone()` would be one
    /// unscrubbed plaintext allocation per keystroke. Only the **clamped suffix**
    /// is stored — the bytes of an oversized paste were already emitted from the
    /// diff, so what is kept is only the next diff's baseline, and storing it whole
    /// would grow the buffers past the capacity [`new`](Self::new) pre-reserved.
    /// [`MIRROR_STORE_CAP`] is one unit past the cap precisely so
    /// [`reanchor_if_needed`](Self::reanchor_if_needed) still sees the overflow.
    /// The history is clamped **before** the push: same result without the
    /// oversized intermediate, and it keeps `head` from underflowing.
    fn set_mirror(&mut self, next: &str) {
        let kept = utf16_suffix(next, MIRROR_STORE_CAP);
        let head = self.history.len() - self.mirror.len();
        self.history.truncate(head);
        clamp_front(
            &mut self.history,
            HISTORY_CAP.saturating_sub(len_utf16(kept)),
        );
        self.history.push_str(kept);
        self.mirror.zeroize();
        self.mirror.push_str(kept);
    }

    /// Re-anchor the window if it has grown past [`MIRROR_CAP`] or shrunk below
    /// [`MIRROR_FLOOR`], reporting whether the caller must re-publish. The shrink
    /// case only fires when history actually has more to give: with nothing left to
    /// refill from, re-anchoring every snapshot would publish forever.
    fn reanchor_if_needed(&mut self) -> bool {
        let len = len_utf16(&self.mirror);
        let over = len > MIRROR_CAP;
        let under = len < MIRROR_FLOOR;
        if !over && !under {
            return false;
        }
        // The tail as a byte range into history, so the refill copies straight
        // between the two scrubbed buffers rather than through a temporary.
        let start = self.history.len() - utf16_suffix(&self.history, MIRROR_TAIL).len();
        if !over && len_utf16(&self.history[start..]) <= len {
            return false;
        }
        self.mirror.zeroize();
        self.mirror.push_str(&self.history[start..]);
        self.reconciles += 1;
        self.revision += 1;
        true
    }

    // -- the platform paths --------------------------------------------------

    /// Reconcile one mobile snapshot ([`ImeEvent::ApplyEditingState`](frust::authoring::ImeEvent))
    /// against the mirror — **the diff engine**. Order matters, and this is it:
    ///
    /// 1. **A live composing range wins**: report, emit nothing, move nothing. Its
    ///    indices are validated first ([`composing_range`]). The post-scrub one shot
    ///    is deliberately **not** spent here: a composition emits nothing, so the
    ///    stale window it guards is still open behind it.
    /// 2. Diff against the mirror, then the desync guards, which all emit nothing,
    ///    move nothing, re-publish and leave the one shot armed: into an armed shot,
    ///    an `added` beginning with the baseline the last scrub dropped
    ///    ([`is_replay`](Self::is_replay)) or bigger than one commit's worth
    ///    ([`is_one_commit`]); and, on any snapshot, an oversized `removed`.
    /// 3. Otherwise a shortening consumes any armed `Key::Named(Backspace)` echo
    ///    first (the dedupe rule), and a snapshot that does not shorten retires it.
    /// 4. Emit the backspaces, then the added text — each through the latch.
    /// 5. A Return in what was added **drops the baseline**; otherwise accept the
    ///    snapshot as the new mirror and re-anchor if the window drifted.
    pub fn on_snapshot(&mut self, state: &EditingState) -> Emission {
        if let Some((base, extent)) = composing_range(state) {
            // Report-only: re-publishing mid-composition would destroy the
            // platform's own composition state.
            let len = i32::try_from(len_utf16(&state.text)).unwrap_or(i32::MAX);
            self.set_composing(
                &state.text,
                (
                    clamp_index(state.selection_base, len),
                    clamp_index(state.selection_extent, len),
                ),
                (base, extent),
            );
            self.revision += 1;
            return Emission::inert(InputClass::Composing);
        }
        self.composing = None;

        let (removed, added) = diff(&self.mirror, &state.text);
        // The one plaintext temporary on this path: owned here so its drop scrubs it.
        let added = Zeroizing::new(added);

        if !added.is_empty() {
            // Into an armed shot the platform may still be holding the line the last
            // scrub dropped, which diffed against nothing reads as one `added` run
            // carrying that whole line — with its Return on iOS, WITHOUT one on
            // Android — and which the oversized-delete guard below cannot catch,
            // since nothing was removed. So it must pass both tests: not BE that
            // line (`is_replay`), and still be at most one commit's worth. The shot
            // is spent here and only here; a REFUSED one re-arms inside `desync`.
            let armed = std::mem::take(&mut self.baseline_dropped);
            if armed {
                if self.is_replay(&added) || !is_one_commit(&added) {
                    return self.desync();
                }
                // Let through: the window is closed, so the fingerprint goes too.
                self.dropped = None;
            }
        }

        if removed > MAX_SNAPSHOT_DELETE {
            // A whole-buffer replacement, not typing: emit NOTHING and re-publish.
            return self.desync();
        }

        let mut backspaces = removed;
        if backspaces > 0 {
            let consumed = backspaces.min(self.pending_backspaces);
            backspaces -= consumed;
            self.pending_backspaces -= consumed;
        } else {
            // The echo never came (or this was an append): retire the arm.
            self.pending_backspaces = 0;
        }

        let mut bytes = Vec::new();
        if backspaces > 0 {
            bytes.extend(self.emit(&vec![BACKSPACE_BYTE; backspaces], ModifierLatch::NONE));
        }
        if !added.is_empty() {
            bytes.extend(self.emit(&text_bytes(&added), ModifierLatch::NONE));
        }

        let class = match (removed > 0, added.as_str()) {
            (_, text) if is_bare_return(text) => InputClass::Enter,
            (_, "") if removed > 0 => InputClass::Backspace,
            (_, "") => InputClass::None,
            _ => InputClass::Text,
        };

        // A submitted line is dropped rather than kept as a baseline, so no `\n` can
        // live in the mirror to be re-emitted later — a multi-line paste included.
        let reconciled = if carries_return(&added) {
            self.submit_line()
        } else {
            self.set_mirror(&state.text);
            self.reanchor_if_needed()
        };
        Emission {
            bytes,
            reconciled,
            class,
        }
    }

    /// A desktop (winit) commit — the composed text the platform finished. Emitted
    /// straight through: there is no platform mirror to diff against on desktop, so
    /// the mirror is left alone — unless the commit carried a Return, which drops
    /// the baseline on every path alike.
    pub fn on_commit(&mut self, text: &str) -> Emission {
        self.composing = None;
        if text.is_empty() {
            return Emission::inert(InputClass::None);
        }
        let class = if is_bare_return(text) {
            InputClass::Enter
        } else {
            InputClass::Text
        };
        let bytes = self.emit(&text_bytes(text), ModifierLatch::NONE);
        let reconciled = carries_return(text) && self.submit_line();
        Emission {
            bytes,
            reconciled,
            class,
        }
    }

    /// A desktop (winit) preedit — reported, never committed (the same rule the
    /// mobile composing branch follows).
    pub fn on_compose(&mut self, text: &str) -> Emission {
        if text.is_empty() {
            self.composing = None;
            return Emission::inert(InputClass::None);
        }
        let units = i32::try_from(len_utf16(text)).unwrap_or(i32::MAX);
        self.set_composing(text, (units, units), (0, units));
        self.revision += 1;
        Emission::inert(InputClass::Composing)
    }

    /// A composition bracket ended (`ImeEvent::Disabled`): drop the composing
    /// echo so [`published`](Self::published) goes back to the mirror.
    pub fn on_composition_end(&mut self) -> Emission {
        if self.composing.take().is_some() {
            self.revision += 1;
        }
        Emission::inert(InputClass::None)
    }

    /// A focus-routed key event: Android's `IME_ACTION_DONE`-as-`Enter`, and every
    /// real key on the desktop preview. The two asymmetric rules land here: a
    /// `Key::Named(Backspace)` emits its byte and **arms the dedupe** so the
    /// reconciler swallows the platform's echo of the same delete, and Android's
    /// Return drops the baseline as iOS's literal `"\n"` does.
    pub fn on_key(&mut self, key: &Key, mods: Modifiers) -> Emission {
        let transient = ModifierLatch::from_modifiers(mods);
        match key {
            Key::Named(named) => {
                if *named == NamedKey::Backspace {
                    self.pending_backspaces =
                        (self.pending_backspaces + 1).min(MAX_PENDING_BACKSPACES);
                }
                let class = match named {
                    NamedKey::Enter => InputClass::Enter,
                    NamedKey::Backspace => InputClass::Backspace,
                    _ => InputClass::Sequence,
                };
                // Through the sequence path, so a hardware Shift+Tab/Shift+↑
                // picks the same variant the bar's own `Shift` does.
                let bytes = self.emit_sequence(named_key_bytes(*named), transient);
                let reconciled = *named == NamedKey::Enter && self.submit_line();
                Emission {
                    bytes,
                    reconciled,
                    class,
                }
            }
            Key::Character(text) => {
                if text.is_empty() {
                    return Emission::inert(InputClass::None);
                }
                let class = if text == "\n" || text == "\r" {
                    InputClass::Enter
                } else {
                    InputClass::Text
                };
                let bytes = self.emit(&text_bytes(text), transient);
                let reconciled = carries_return(text) && self.submit_line();
                Emission {
                    bytes,
                    reconciled,
                    class,
                }
            }
        }
    }

    /// A key-bar tap: a latch toggle, or a control sequence sent through the same
    /// choke point every other emission uses.
    pub fn on_bar_key(&mut self, key: BarKey) -> Emission {
        match key.sequence() {
            None => {
                match key {
                    BarKey::Ctrl => self.latch.ctrl = !self.latch.ctrl,
                    BarKey::Alt => self.latch.alt = !self.latch.alt,
                    BarKey::Shift => self.latch.shift = !self.latch.shift,
                    // Unreachable: only the three latch keys have no sequence.
                    _ => {}
                }
                self.revision += 1;
                Emission::inert(InputClass::Latch)
            }
            Some(sequence) => {
                let bytes = self.emit_sequence(sequence, ModifierLatch::NONE);
                Emission {
                    bytes,
                    reconciled: false,
                    class: InputClass::Sequence,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::harness::{hex, snapshot, type_text};
    use super::*;

    // The diff engine.

    #[test]
    fn one_appended_character_emits_one_byte() {
        let mut source = KeystrokeSource::new();
        let emission = source.on_snapshot(&snapshot("a"));
        assert_eq!(emission.bytes, b"a");
        assert_eq!(emission.class, InputClass::Text);
        assert!(!emission.reconciled, "no re-anchor this early");
    }

    #[test]
    fn a_swipe_typed_word_arrives_as_one_contiguous_burst() {
        // The zero-composing norm: one multi-character commit, one burst.
        let mut source = KeystrokeSource::new();
        let emission = source.on_snapshot(&snapshot("hello"));
        assert_eq!(emission.bytes, b"hello");
        assert_eq!(emission.class, InputClass::Text);
    }

    #[test]
    fn repeated_identical_characters_each_emit() {
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "aaa")), "616161");
    }

    #[test]
    fn a_shortened_snapshot_emits_one_backspace_per_removed_unit() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "abc");

        let one = source.on_snapshot(&snapshot("ab"));
        assert_eq!(hex(&one.bytes), "7f");
        assert_eq!(one.class, InputClass::Backspace);

        // A run: the platform collapsed two deletes into one snapshot.
        let run = source.on_snapshot(&snapshot(""));
        assert_eq!(hex(&run.bytes), "7f7f");
    }

    #[test]
    fn a_replaced_span_emits_the_backspaces_then_the_bytes() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "abc");
        let emission = source.on_snapshot(&snapshot("axyc"));
        assert_eq!(hex(&emission.bytes), "7f7879", "one delete, then 'xy'");
    }

    #[test]
    fn ios_delivers_return_as_a_literal_newline_and_it_emits_cr() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        let emission = source.on_snapshot(&snapshot("ls\n"));
        assert_eq!(hex(&emission.bytes), "0d", "CR, never LF");
        assert_eq!(emission.class, InputClass::Enter);

        // A submitted line is dropped rather than kept as the next diff's baseline,
        // so iOS's accumulated `\n` can never be part of a later bulk re-emission.
        assert!(emission.reconciled, "the window re-anchors, to empty");
        assert_eq!(source.mirror(), "", "no `\\n` is left in the mirror");
        assert_eq!(source.buffered_units(), 0, "…and none in history either");
    }

    #[test]
    fn the_keystroke_after_a_return_diffs_against_an_empty_baseline() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        source.on_snapshot(&snapshot("ls\n"));

        // The platform reconciled to the republished (empty) mirror inside the same
        // round trip, so its next snapshot is built on nothing.
        assert_eq!(
            hex(&type_text(&mut source, "x")),
            "78",
            "one byte, not `lsx`"
        );
        assert_eq!(source.mirror(), "x");
    }

    #[test]
    fn a_multi_line_paste_sends_every_return_and_still_leaves_no_newline() {
        let mut source = KeystrokeSource::new();
        let emission = source.on_snapshot(&snapshot("a\nb"));
        assert_eq!(hex(&emission.bytes), "610d62", "'a', CR, 'b'");
        assert!(emission.reconciled);
        assert_eq!(source.buffered_units(), 0, "the whole paste is dropped");
    }

    #[test]
    fn android_delivers_return_as_a_key_event_and_it_emits_the_same_cr() {
        let mut source = KeystrokeSource::new();
        let emission = source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert_eq!(hex(&emission.bytes), "0d");
        assert_eq!(emission.class, InputClass::Enter);
    }

    #[test]
    fn an_android_return_key_drops_the_baseline_the_same_way_ios_does() {
        // The secret typed before `IME_ACTION_DONE` must not survive it just
        // because this platform reports Return as a key rather than a `\n`.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "hunter2");
        assert!(source.buffered_units() > 0);

        let emission = source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert!(emission.reconciled, "one restartInput, on the rarest key");
        assert_eq!(source.buffered_units(), 0);
        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    // The stale-snapshot guard: the frames the PLATFORM may still be holding
    // the submitted line for.

    #[test]
    fn a_stale_snapshot_after_a_return_replays_nothing_to_the_host() {
        let mut source = KeystrokeSource::new();
        // Positive precondition: the line really was typed, and really was sent.
        assert_eq!(hex(&type_text(&mut source, "ls")), "6c73");
        let submitted = source.on_snapshot(&snapshot("ls\n"));
        assert_eq!(hex(&submitted.bytes), "0d", "the Return went out");
        assert_eq!(source.mirror(), "", "…and guard 1 dropped the baseline");
        let sent = source.emitted();

        // THE window. This snapshot is built from the PLATFORM's own stale buffer —
        // deliberately not from `source.mirror()`, which is what would hide the bug.
        // Diffed against the empty baseline it reads `added = "ls\n"`, whose `\n`
        // `text_bytes` maps to CR: emitted, `ls` runs again on the remote host.
        let stale = source.on_snapshot(&snapshot("ls\n"));
        assert!(
            stale.bytes.is_empty(),
            "NOTHING is re-emitted, got {}",
            hex(&stale.bytes)
        );
        assert_eq!(stale.class, InputClass::Desync);
        assert!(stale.reconciled, "…and the caller re-publishes");
        assert_eq!(source.emitted(), sent, "not one byte reached the session");
        assert_eq!(
            source.published().text,
            "",
            "the empty baseline is what the platform is pulled back to"
        );
    }

    #[test]
    fn the_stale_android_shape_carries_no_newline_and_is_still_caught() {
        // THE Android case, and the reason the guard cannot be keyed on the Return:
        // `IME_ACTION_DONE` leaves as an editor action, never as text, and
        // `performEditorAction` does not re-poll — so the `Editable` keeps the
        // pre-submit line, WITHOUT a newline, for several frames.
        let mut source = KeystrokeSource::new();
        // Positive preconditions: the line really was typed, and the Return
        // really went out, before there is any stale window to speak of.
        assert_eq!(hex(&type_text(&mut source, "sudo -i")), "7375646f202d69");
        assert_eq!(
            hex(&source
                .on_key(&Key::Named(NamedKey::Enter), Modifiers::default())
                .bytes),
            "0d"
        );
        assert_eq!(source.mirror(), "", "…leaving an empty baseline, armed");
        let sent = source.emitted();

        // RAW: built from the PLATFORM's own stale buffer — a snapshot built from
        // our own mirror is what hid this shape in the first place.
        let stale = source.on_snapshot(&snapshot("sudo -i"));
        assert!(
            stale.bytes.is_empty(),
            "a missing newline is no excuse, got {}",
            hex(&stale.bytes)
        );
        assert_eq!(stale.class, InputClass::Desync);
        assert!(stale.reconciled, "…and the caller re-publishes");
        assert_eq!(source.emitted(), sent, "not one byte reached the session");
        assert_eq!(
            source.published().text,
            "",
            "the empty baseline is what the platform is pulled back to"
        );
    }

    #[test]
    fn the_stale_ios_shape_carries_the_return_and_is_caught_separately() {
        // The other platform's shape of the same window: iOS accumulates the literal
        // Return in its mirror, so the stale buffer arrives WITH it.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "sudo -i")), "7375646f202d69");
        assert_eq!(
            hex(&source.on_snapshot(&snapshot("sudo -i\r")).bytes),
            "0d",
            "the Return went out"
        );
        assert_eq!(source.mirror(), "", "…leaving an empty baseline, armed");
        let sent = source.emitted();

        let stale = source.on_snapshot(&snapshot("sudo -i\r"));
        assert!(
            stale.bytes.is_empty(),
            "no replay from the iOS shape either"
        );
        assert_eq!(stale.class, InputClass::Desync);
        assert!(stale.reconciled);
        assert_eq!(source.emitted(), sent);
    }

    #[test]
    fn genuine_first_input_after_a_return_still_lands() {
        // The guard admits one commit's worth, so every shape a real first keystroke
        // has survives it: one character, one emoji tap (two units, one commit), and
        // one gesture-typed word with or without its trailing space.
        let cases: [(&str, &str); 4] = [
            ("x", "78"),
            ("\u{1f600}", "f09f9880"),
            ("status", "737461747573"),
            ("status ", "73746174757320"),
        ];
        for (first, expected) in cases {
            let mut source = KeystrokeSource::new();
            assert_eq!(hex(&type_text(&mut source, "ls")), "6c73");
            assert_eq!(hex(&source.on_snapshot(&snapshot("ls\n")).bytes), "0d");
            assert_eq!(source.mirror(), "", "armed, on an empty baseline");

            // RAW again: the platform reconciled inside the same round trip.
            let emission = source.on_snapshot(&snapshot(first));
            assert_eq!(hex(&emission.bytes), expected, "{first:?} must land");
            assert_eq!(emission.class, InputClass::Text);
            assert_eq!(source.mirror(), first, "…and become the new baseline");
        }
    }

    #[test]
    fn a_no_op_snapshot_does_not_spend_the_stale_shot() {
        // Gboard's `finishComposingText()` pushes the mirror unchanged: a snapshot
        // that adds nothing and closes no window. Spending the shot on it would
        // retire the guard while its precondition — the empty baseline — still held.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "ls")), "6c73");
        assert_eq!(hex(&source.on_snapshot(&snapshot("ls\n")).bytes), "0d");
        assert_eq!(source.mirror(), "", "the baseline was dropped");
        let sent = source.emitted();

        let no_op = source.on_snapshot(&snapshot(""));
        assert!(no_op.bytes.is_empty());
        assert_eq!(no_op.class, InputClass::None);
        assert_eq!(source.emitted(), sent, "a positive precondition: no-op");

        // The shot is still armed, so the stale line behind it is still caught.
        let stale = source.on_snapshot(&snapshot("ls -la"));
        assert!(
            stale.bytes.is_empty(),
            "the shot was spent on a no-op, got {}",
            hex(&stale.bytes)
        );
        assert_eq!(stale.class, InputClass::Desync);
        assert!(stale.reconciled);
        assert_eq!(source.emitted(), sent, "not one byte reached the session");
    }

    #[test]
    fn the_one_commit_test_admits_a_keystroke_and_rejects_a_line() {
        // The admission rule itself, as a table — what the guard above is.
        let cases: [(&str, bool, &str); 12] = [
            ("x", true, "one character"),
            (" ", true, "…whatever character it is"),
            ("-", true, "…including shell punctuation"),
            ("\u{1f600}", true, "an emoji tap is two units, one commit"),
            ("\n", true, "a bare Return has no command text to replay"),
            ("\r\n", true, "…in either spelling"),
            ("status", true, "one gesture-typed word"),
            ("status ", true, "…plus the space a gesture commit appends"),
            ("ls -la", false, "a shell line's whitespace gives it away"),
            ("cd /tmp", false, "…as does its punctuation"),
            ("ls\n", false, "a Return WITH text is the stale-line shape"),
            (
                "abcdefghijklmnopq",
                false,
                "one unit past the bound is no longer one commit",
            ),
        ];
        for (added, expected, why) in cases {
            assert_eq!(is_one_commit(added), expected, "{added:?}: {why}");
        }
        assert_eq!(
            len_utf16("abcdefghijklmnopq"),
            MAX_POST_SCRUB_ADD + 1,
            "the long case really is one past the bound"
        );
    }

    #[test]
    fn the_stale_guard_is_one_shot_and_never_suppresses_ordinary_typing() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        source.on_snapshot(&snapshot("ls\n"));

        // The first ordinary snapshot spends the shot…
        assert_eq!(
            hex(&type_text(&mut source, "x")),
            "78",
            "typing still lands"
        );
        // …so the next Return is a Return again, not a suppressed desync.
        let second = source.on_snapshot(&snapshot("x\n"));
        assert_eq!(hex(&second.bytes), "0d", "the next line still submits");
        assert_eq!(second.class, InputClass::Enter);
        // And a stale replay of THAT line is caught by the freshly armed shot.
        let stale = source.on_snapshot(&snapshot("x\n"));
        assert!(stale.bytes.is_empty());
        assert_eq!(stale.class, InputClass::Desync);
    }

    #[test]
    fn a_return_and_nothing_else_is_let_through_after_a_return() {
        // A blank line at a shell prompt: Return pressed twice. The second carries no
        // command text to replay, so the guard's narrowing lets it through.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        assert_eq!(hex(&source.on_snapshot(&snapshot("ls\n")).bytes), "0d");

        let blank = source.on_snapshot(&snapshot("\n"));
        assert_eq!(hex(&blank.bytes), "0d", "the blank line still submits");
        assert_eq!(blank.class, InputClass::Enter);
        assert_eq!(source.buffered_units(), 0, "and still keeps nothing");
    }

    // The guard is more than one snapshot deep, and compares CONTENT, not shape.

    #[test]
    fn a_second_stale_snapshot_in_the_same_window_replays_nothing_either() {
        // THE multi-frame case. `performEditorAction` does not re-poll, so the SAME
        // stale `Editable` can be polled on several frames. A guard that spent its
        // shot on the first leaves the second unguarded — `armed == false` and
        // `removed == 0` — and the whole previous command line reaches the host.
        let mut source = KeystrokeSource::new();
        // Positive preconditions: the line was typed, and the Return went out.
        assert_eq!(hex(&type_text(&mut source, "sudo -i")), "7375646f202d69");
        assert_eq!(
            hex(&source
                .on_key(&Key::Named(NamedKey::Enter), Modifiers::default())
                .bytes),
            "0d"
        );
        assert_eq!(source.mirror(), "", "armed, on an empty baseline");
        let sent = source.emitted();

        // RAW, twice: the platform's own stale buffer, polled on two frames.
        for round in 1..=2 {
            let stale = source.on_snapshot(&snapshot("sudo -i"));
            assert!(
                stale.bytes.is_empty(),
                "round {round} replayed {}",
                hex(&stale.bytes)
            );
            assert_eq!(stale.class, InputClass::Desync, "round {round}");
            assert!(stale.reconciled, "round {round} must re-publish");
            assert_eq!(
                source.emitted(),
                sent,
                "round {round}: bytes reached the session"
            );
        }
        assert_eq!(
            source.published().text,
            "",
            "the empty baseline is still what the platform is pulled back to"
        );

        // …and the guard still lets the user type once the platform catches up.
        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn an_oversized_delete_desync_leaves_the_guard_armed_for_the_frame_after_it() {
        // The other branch of the same rule: a refusal arms the shot wherever it
        // fires, so a platform that has just proved itself out of step gets one
        // commit's worth at a time until a snapshot the source can reconcile arrives.
        let mut source = KeystrokeSource::new();
        let typed = "a".repeat(MIRROR_CAP / 2);
        type_text(&mut source, &typed);
        let sent = source.emitted();
        assert_eq!(
            sent,
            typed.len(),
            "a positive precondition: it all went out"
        );

        // One unit past the delete bound: refused, and the mirror does not move.
        let over = &typed[..typed.len() - (MAX_SNAPSHOT_DELETE + 1)];
        let refused = source.on_snapshot(&snapshot(over));
        assert_eq!(refused.class, InputClass::Desync);
        assert_eq!(source.mirror(), typed, "the baseline does not move");

        // The frame AFTER it is still guarded: a bulk add — what a bridge one
        // frame behind reports — emits nothing rather than reaching the host.
        let mut bulk = typed.clone();
        bulk.push_str("wholly different text");
        let after = source.on_snapshot(&snapshot(&bulk));
        assert!(
            after.bytes.is_empty(),
            "the frame after a desync was unguarded: {}",
            hex(&after.bytes)
        );
        assert_eq!(after.class, InputClass::Desync);
        assert_eq!(source.emitted(), sent, "not one byte reached the session");

        // …and ordinary typing still lands, retiring the arm.
        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn a_word_shaped_stale_line_is_refused_on_its_content_not_its_shape() {
        // THE content-test case. On Android the stale buffer is the pre-submit line
        // WITHOUT a newline, so `"hunter2"` is exactly the shape `is_one_commit`
        // exists to admit. What refuses it is that it IS the line the scrub dropped.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "hunter2")), "68756e74657232");
        assert_eq!(
            hex(&source
                .on_key(&Key::Named(NamedKey::Enter), Modifiers::default())
                .bytes),
            "0d"
        );
        assert_eq!(source.mirror(), "", "armed, on an empty baseline");
        // The positive precondition that makes this test mean anything: by
        // SHAPE this is one ordinary gesture commit, and always was.
        assert!(
            is_one_commit("hunter2"),
            "the shape rule admits it — the content rule is what must not"
        );
        let sent = source.emitted();

        let stale = source.on_snapshot(&snapshot("hunter2"));
        assert!(
            stale.bytes.is_empty(),
            "the password replayed to the host: {}",
            hex(&stale.bytes)
        );
        assert_eq!(stale.class, InputClass::Desync);
        assert!(stale.reconciled, "…and the caller re-publishes");
        assert_eq!(source.emitted(), sent, "not one byte reached the session");
        assert_eq!(source.published().text, "");
    }

    #[test]
    fn a_stale_line_plus_a_character_typed_during_the_window_is_refused_whole() {
        // The platform is behind AND the user kept typing, so the stale buffer is the
        // dropped line with a genuine keystroke on the end. The prefix digest sees
        // it; the decision is to refuse the whole snapshot rather than emit the `x`
        // alone — a fragment of a keystroke run in a shell is worse than a dropped one.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "hunter2")), "68756e74657232");
        assert_eq!(
            hex(&source
                .on_key(&Key::Named(NamedKey::Enter), Modifiers::default())
                .bytes),
            "0d"
        );
        assert!(
            is_one_commit("hunter2x"),
            "a positive precondition: by shape this is one commit too"
        );
        let sent = source.emitted();

        let stale = source.on_snapshot(&snapshot("hunter2x"));
        assert!(
            stale.bytes.is_empty(),
            "replay-plus-suffix emitted {}",
            hex(&stale.bytes)
        );
        assert_eq!(stale.class, InputClass::Desync);
        assert_eq!(source.emitted(), sent, "not one byte reached the session");

        // The cost of that decision, and its recovery: the `x` was dropped with the
        // replay, and retyping it onto the re-published baseline lands.
        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn a_genuine_gesture_word_after_a_word_shaped_line_still_lands() {
        // The digest refuses OUR OWN dropped line, not every word: a different
        // gesture commit of exactly the same shape is ordinary first input.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "hunter2")), "68756e74657232");
        assert_eq!(
            hex(&source
                .on_key(&Key::Named(NamedKey::Enter), Modifiers::default())
                .bytes),
            "0d"
        );
        assert!(
            source.dropped.is_some(),
            "a positive precondition: the digest really is armed"
        );

        let emission = source.on_snapshot(&snapshot("status"));
        assert_eq!(
            hex(&emission.bytes),
            "737461747573",
            "a genuine swipe word must still land"
        );
        assert_eq!(emission.class, InputClass::Text);
        assert_eq!(source.mirror(), "status", "…and become the new baseline");
        assert!(
            source.dropped.is_none(),
            "the spent shot takes the dropped line's fingerprint with it"
        );
    }

    #[test]
    fn the_replay_digest_is_seeded_per_instance_and_keeps_no_plaintext() {
        // Two sources, the same submitted line: the recorded lengths match (that part
        // IS the line's length), and the digests do not, because the seed is this
        // instance's own `RandomState` rather than a fixed constant.
        let line = "hunter2";
        let mut first = KeystrokeSource::new();
        let mut second = KeystrokeSource::new();
        for source in [&mut first, &mut second] {
            type_text(source, line);
            source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        }

        let (first_digest, first_units) = first.dropped.expect("the dropped line is recorded");
        let (second_digest, second_units) = second.dropped.expect("…in both sources");
        assert_eq!(
            (first_units, second_units),
            (len_utf16(line), len_utf16(line)),
            "a length and a digest is ALL that is kept"
        );
        assert_ne!(
            first_digest, second_digest,
            "two sources digest the same line identically: the seed is fixed"
        );
        // Not one shared-seed accident either: several probes, all disagreeing.
        for probe in ["", "a", "ls -la", "hunter2x"] {
            assert_ne!(
                first.digest(probe),
                second.digest(probe),
                "{probe:?} digests the same in both sources"
            );
        }
        // Each source's own digest is stable for its own seed — which is what
        // makes `is_replay` a test at all.
        assert_eq!(first.digest(line), first_digest);
        // And nothing plaintext survived the scrub the submit performed.
        assert_eq!(first.buffered_units(), 0);
        assert_eq!(first.published().text, "");
    }

    #[test]
    fn a_one_keystroke_baseline_records_no_digest_and_stays_typeable() {
        // The bound on the content test, and the residual it leaves.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert!(
            len_utf16("ls") < MIN_REPLAY_DIGEST_UNITS,
            "`ls` really is inside one keystroke's worth"
        );
        assert!(
            source.dropped.is_none(),
            "no digest at one keystroke's worth: `ls` stays the documented residual"
        );

        // Past the bound, a digest IS recorded — and refuses the replay that
        // the bare-word shape admits.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "htop");
        source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert!(len_utf16("htop") >= MIN_REPLAY_DIGEST_UNITS);
        assert_eq!(
            source.dropped.map(|(_, units)| units),
            Some(len_utf16("htop")),
            "`htop` is past the bound, so its digest is kept"
        );
        assert!(is_one_commit("htop"), "a positive precondition: bare word");
        let stale = source.on_snapshot(&snapshot("htop"));
        assert!(stale.bytes.is_empty(), "got {}", hex(&stale.bytes));
        assert_eq!(stale.class, InputClass::Desync);

        // And the reason the bound is where it is: the identical NEXT keystroke. A
        // digest at this length would refuse it every time, since a refusal re-arms,
        // so `y` at a `[y/n]` prompt would be untypeable.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "y");
        source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert_eq!(hex(&type_text(&mut source, "y")), "79", "`y` still types");
    }

    #[test]
    fn a_blur_arms_the_same_shot_a_return_does() {
        // Every scrub boundary drops the baseline, so every one of them opens
        // the same window onto a platform buffer that has not caught up.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "secret");
        source.scrub();

        let stale = source.on_snapshot(&snapshot("secret\n"));
        assert!(stale.bytes.is_empty(), "no replay after a blur either");
        assert_eq!(stale.class, InputClass::Desync);
    }

    // The desync guard, and the scrub.

    #[test]
    fn a_whole_buffer_replacement_emits_nothing_and_re_publishes() {
        // Dictation rewriting the field per partial, a replace-span rewrite, or a
        // bridge one frame behind all arrive as one snapshot that deletes the buffer.
        // Emitting that is how previously typed commands reach the remote host.
        let mut source = KeystrokeSource::new();
        let typed = "rm -rf /tmp/x";
        type_text(&mut source, typed);
        let before = source.emitted();

        let emission = source.on_snapshot(&snapshot("something else entirely"));
        assert!(emission.bytes.is_empty(), "NOTHING is emitted");
        assert_eq!(emission.class, InputClass::Desync);
        assert!(emission.reconciled, "…and the caller re-publishes");
        assert_eq!(source.emitted(), before, "not one byte reached the session");
        assert_eq!(
            source.published().text,
            typed,
            "the re-published state is the mirror the source still believes in"
        );
    }

    #[test]
    fn a_delete_run_up_to_the_threshold_still_emits_and_past_it_does_not() {
        let mut source = KeystrokeSource::new();
        let typed = "a".repeat(MIRROR_CAP / 2);
        type_text(&mut source, &typed);

        // Exactly at the threshold: a real (if unusually collapsed) delete run.
        let keep = typed.len() - MAX_SNAPSHOT_DELETE;
        let emission = source.on_snapshot(&snapshot(&typed[..keep]));
        assert_eq!(emission.bytes.len(), MAX_SNAPSHOT_DELETE);
        assert_eq!(emission.class, InputClass::Backspace);

        // One unit past it: read as a desync instead, mirror untouched.
        let mirror = source.mirror().to_string();
        let shorter = &mirror[..mirror.len() - (MAX_SNAPSHOT_DELETE + 1)];
        let emission = source.on_snapshot(&snapshot(shorter));
        assert!(emission.bytes.is_empty());
        assert_eq!(emission.class, InputClass::Desync);
        assert_eq!(source.mirror(), mirror, "the baseline does not move");
    }

    #[test]
    fn scrub_empties_both_buffers_and_the_composing_echo() {
        let mut source = KeystrokeSource::new();
        // Past one trim, so history holds more than the mirror does.
        type_text(&mut source, &"abcdefghij".repeat(12));
        source.on_compose("ni");
        assert!(source.buffered_units() > 0);
        assert!(source.is_composing());

        source.scrub();

        assert_eq!(source.buffered_units(), 0, "nothing retained");
        assert!(!source.is_composing());
        assert_eq!(source.published().text, "", "and nothing left to publish");
        // The latch is not a secret and deliberately survives (see `scrub`).
        assert_eq!(hex(&type_text(&mut source, "z")), "7a");
    }

    #[test]
    fn the_composing_echo_is_retained_typing_and_is_scrubbed_as_such() {
        // A composition ALONE, so what is asserted is the echo's own retention: it
        // holds the whole platform mirror, a password mid-composition included.
        let mut source = KeystrokeSource::new();
        source.on_compose("hunter");
        assert!(source.is_composing(), "a positive precondition: it is live");
        assert_eq!(source.published().text, "hunter", "…and echoed verbatim");
        assert_eq!(
            source.buffered_units(),
            6,
            "the echo counts as retained typing, like every other buffer"
        );

        source.scrub();

        assert!(!source.is_composing());
        assert_eq!(
            source.buffered_units(),
            0,
            "the echo goes with the buffers, and its `Zeroizing` drop scrubs it"
        );
        assert_eq!(source.published().text, "");
    }

    #[test]
    fn a_composing_snapshot_replaces_the_previous_one_leaving_no_tail() {
        // The echo's buffer is rewritten in place rather than reallocated, so a
        // shrinking composition must not leave the longer one's tail behind.
        let mut source = KeystrokeSource::new();
        source.on_compose("nih");
        assert_eq!(source.published().text, "nih");
        assert_eq!(source.published().composing_extent, 3);

        source.on_compose("n");
        assert_eq!(source.published().text, "n", "no stale tail survives");
        assert_eq!(source.published().composing_extent, 1);
        assert_eq!(source.buffered_units(), 1);

        // …and the composition still commits through the ordinary diff path.
        source.on_composition_end();
        assert_eq!(
            hex(&source.on_snapshot(&snapshot("\u{4f60}")).bytes),
            "e4bda0"
        );
    }

    #[test]
    fn a_key_backspace_and_its_echoed_shortening_emit_exactly_one_byte() {
        // Android's shape: the key event AND the mirror shortening.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ab");

        let key = source.on_key(&Key::Named(NamedKey::Backspace), Modifiers::default());
        assert_eq!(hex(&key.bytes), "7f");
        let echo = source.on_snapshot(&snapshot("a"));
        assert!(echo.bytes.is_empty(), "the echo is swallowed, not doubled");
        assert_eq!(source.mirror(), "a", "…but the mirror still follows it");
    }

    #[test]
    fn a_snapshot_only_shortening_emits_exactly_one_byte() {
        // iOS's shape: no key event at all.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ab");
        assert_eq!(hex(&source.on_snapshot(&snapshot("a")).bytes), "7f");
    }

    #[test]
    fn an_unechoed_key_backspace_cannot_swallow_a_later_real_delete() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ab");
        source.on_key(&Key::Named(NamedKey::Backspace), Modifiers::default());
        // The echo never came; an append retires the arm…
        assert_eq!(hex(&source.on_snapshot(&snapshot("abc")).bytes), "63");
        // …so this genuine delete still emits.
        assert_eq!(hex(&source.on_snapshot(&snapshot("ab")).bytes), "7f");
    }

    #[test]
    fn a_surrogate_pair_emits_its_utf8_and_is_never_split() {
        let mut source = KeystrokeSource::new();
        let emission = source.on_snapshot(&snapshot("a\u{1f600}"));
        assert_eq!(hex(&emission.bytes), "61f09f9880");
        // Deleting it costs two units — the platform's own accounting.
        let deleted = source.on_snapshot(&snapshot("a"));
        assert_eq!(hex(&deleted.bytes), "7f7f");
    }

    #[test]
    fn quotes_and_dashes_are_never_transformed() {
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&source.on_snapshot(&snapshot("\"")).bytes), "22");
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&source.on_snapshot(&snapshot("--")).bytes), "2d2d");
    }

    #[test]
    fn a_composing_range_is_reported_and_never_committed() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "a");
        let composing = EditingState {
            text: "ani".to_string(),
            selection_base: 3,
            selection_extent: 3,
            composing_base: 1,
            composing_extent: 3,
        };
        let emission = source.on_snapshot(&composing);
        assert!(emission.bytes.is_empty(), "marked text is not committed");
        assert_eq!(emission.class, InputClass::Composing);
        assert!(!emission.reconciled, "and it must NEVER re-publish");
        assert_eq!(source.mirror(), "a", "the baseline does not move");
        assert_eq!(
            source.published(),
            composing,
            "the published state echoes the composition verbatim, so a \
             paint-pass republish cannot destroy it"
        );

        // Committing it diffs against the untouched baseline: one burst.
        let committed = source.on_snapshot(&snapshot("a\u{4f60}"));
        assert_eq!(hex(&committed.bytes), "e4bda0");
    }

    #[test]
    fn a_composing_range_is_validated_before_it_is_echoed_back() {
        // The one thing this module hands the platform verbatim, so the indices are
        // checked against the text that arrived with them rather than trusted.
        let out_of_range = EditingState {
            text: "ab".to_string(),
            selection_base: 99,
            selection_extent: 99,
            composing_base: 1,
            composing_extent: 99,
        };
        let mut source = KeystrokeSource::new();
        source.on_snapshot(&out_of_range);
        let published = source.published();
        assert_eq!(published.composing_extent, 2, "clamped to the text's end");
        assert_eq!(published.composing_base, 1);
        assert_eq!(published.selection_base, 2, "…and so is the caret");
        assert_eq!(published.selection_extent, 2);

        // An inverted/unusable range is not a composition at all: the snapshot takes
        // the ordinary diff path. Nothing is invented either way.
        let inverted = EditingState {
            text: "ab".to_string(),
            selection_base: -1,
            selection_extent: -1,
            composing_base: 2,
            composing_extent: 1,
        };
        let mut source = KeystrokeSource::new();
        let emission = source.on_snapshot(&inverted);
        assert!(!source.is_composing());
        assert_eq!(hex(&emission.bytes), "6162");
    }

    #[test]
    fn an_unchanged_snapshot_is_our_own_echo_and_emits_nothing() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ab");
        let emission = source.on_snapshot(&snapshot("ab"));
        assert!(emission.bytes.is_empty());
        assert_eq!(emission.class, InputClass::None);
        assert!(emission.is_empty());
    }

    // The rolling window.

    #[test]
    fn the_published_state_collapses_the_caret_at_the_mirror_end() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "abc");
        let published = source.published();
        assert_eq!(published.text, "abc");
        assert_eq!(published.selection_base, 3);
        assert_eq!(published.selection_extent, 3);
        assert_eq!(published.composing_base, -1);
        assert_eq!(published.composing_extent, -1);
    }

    #[test]
    fn typing_past_the_cap_re_anchors_the_window_once_per_trim() {
        let mut source = KeystrokeSource::new();
        let typed = 300usize;
        let bytes = type_text(&mut source, &"a".repeat(typed));
        assert_eq!(bytes.len(), typed, "every keystroke still emitted");

        assert!(
            len_utf16(source.mirror()) <= MIRROR_CAP,
            "the mirror stays bounded: {}",
            len_utf16(source.mirror())
        );
        // One re-anchor per `CAP - TAIL` typed units, and nothing like one per
        // keystroke (which is what a platform `restartInput` storm would be).
        let expected = typed / (MIRROR_CAP - MIRROR_TAIL);
        assert!(
            source.reconciles() <= expected as u64 + 1,
            "{} re-anchors for {typed} keystrokes",
            source.reconciles()
        );
        assert!(source.reconciles() >= 1, "the cap must actually have fired");
    }

    #[test]
    fn the_window_is_re_anchored_from_history_rather_than_left_empty() {
        // Type past one trim, then delete the whole visible window: the source
        // refills it from the user's own history so backspace keeps emitting.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, &"abcdefghij".repeat(12));
        assert!(source.reconciles() >= 1);

        let mut deletes = 0usize;
        for _ in 0..40 {
            let mirror = source.mirror().to_string();
            let Some(shorter) = mirror
                .char_indices()
                .next_back()
                .map(|(at, _)| &mirror[..at])
            else {
                break;
            };
            let emission = source.on_snapshot(&snapshot(shorter));
            deletes += emission.bytes.len();
        }
        assert_eq!(deletes, 40, "every delete emitted exactly one byte");
        assert!(
            !source.mirror().is_empty(),
            "the window is never left empty while history has more to give"
        );
    }

    #[test]
    fn an_oversized_snapshot_is_never_stored_whole() {
        // `set_mirror` is called directly because the re-anchor immediately after it
        // would hide what is under test: a snapshot bigger than the pre-reserved
        // capacity, stored whole, reallocates — freeing plaintext no scrub reaches.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "abc");
        assert_eq!(source.mirror(), "abc", "a positive precondition");
        let (mirror_capacity, history_capacity) =
            (source.mirror.capacity(), source.history.capacity());

        let pasted = "s".repeat(MIRROR_CAP * 4);
        source.set_mirror(&pasted);

        assert_eq!(
            len_utf16(source.mirror()),
            MIRROR_STORE_CAP,
            "only the clamped suffix is stored"
        );
        assert!(
            len_utf16(&source.history) <= HISTORY_CAP,
            "…and the history stays inside its own bound: {}",
            len_utf16(&source.history)
        );
        assert_eq!(
            (source.mirror.capacity(), source.history.capacity()),
            (mirror_capacity, history_capacity),
            "neither buffer reallocated, so no plaintext was freed unscrubbed"
        );
        assert!(
            source.history.ends_with(source.mirror()),
            "the accounting invariant holds: the mirror is a suffix of history"
        );

        // Still overflowing by one unit, so the re-anchor still fires — the
        // whole reason the clamp is one PAST the cap.
        assert!(source.reanchor_if_needed(), "the window still re-anchors");
        assert_eq!(len_utf16(source.mirror()), MIRROR_TAIL);
        // And the next snapshot folds in without underflowing `head`.
        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn an_oversized_paste_emits_every_byte_before_it_is_clamped_away() {
        // The clamp is a storage bound, never an emission one: the bytes go
        // out from the diff, and only the next diff's baseline is trimmed.
        let mut source = KeystrokeSource::new();
        let pasted = "s".repeat(MIRROR_CAP * 4);
        let emission = source.on_snapshot(&snapshot(&pasted));
        assert_eq!(emission.bytes.len(), pasted.len(), "every byte was sent");
        assert!(emission.reconciled, "…and the window re-anchored");
        assert_eq!(len_utf16(source.mirror()), MIRROR_TAIL);
    }

    #[test]
    fn a_re_anchor_reports_itself_so_the_caller_republishes() {
        let mut source = KeystrokeSource::new();
        let mut buffer = String::new();
        let mut reported = 0usize;
        for _ in 0..(MIRROR_CAP + 8) {
            buffer.push('x');
            if source.on_snapshot(&snapshot(&buffer)).reconciled {
                reported += 1;
                buffer = source.mirror().to_string();
            }
        }
        assert_eq!(reported as u64, source.reconciles());
        assert_eq!(reported, 1, "exactly one publish for one trim");
    }

    #[test]
    fn a_re_anchor_publishes_the_trimmed_tail_of_what_was_typed() {
        let mut source = KeystrokeSource::new();
        // Exactly one unit past the cap, so the trim is the last thing to fire.
        let typed: String = (0..=MIRROR_CAP)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        type_text(&mut source, &typed);
        assert_eq!(source.reconciles(), 1);

        let published = source.published();
        assert_eq!(len_utf16(&published.text), MIRROR_TAIL);
        assert_eq!(
            published.text,
            utf16_suffix(&typed, MIRROR_TAIL),
            "the republished window is the user's own text, never an invented one"
        );
        assert_eq!(
            published.selection_base, MIRROR_TAIL as i32,
            "the caret is collapsed at its end"
        );
    }

    #[test]
    fn there_is_no_sentinel_character_in_the_published_state() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls -la");
        assert_eq!(source.published().text, "ls -la");
        assert!(
            !source.published().text.contains('\u{200b}'),
            "no invented character ever enters the mirror"
        );
    }

    // Desktop paths.

    #[test]
    fn a_desktop_commit_emits_straight_through() {
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&source.on_commit("hi").bytes), "6869");
        assert_eq!(
            source.on_commit("\n").class,
            InputClass::Enter,
            "a lone newline commit is Return"
        );
        assert_eq!(source.mirror(), "", "desktop has no mirror to move");
    }

    #[test]
    fn a_desktop_preedit_is_reported_and_cleared_by_its_bracket() {
        let mut source = KeystrokeSource::new();
        let emission = source.on_compose("ni");
        assert!(emission.bytes.is_empty());
        assert!(source.is_composing());
        assert_eq!(source.published().text, "ni", "echoed verbatim");
        source.on_composition_end();
        assert!(!source.is_composing());
        assert_eq!(source.published().text, "");
    }

    #[test]
    fn a_hardware_modifier_applies_to_its_own_event_only() {
        let mut source = KeystrokeSource::new();
        let mods = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let emission = source.on_key(&Key::Character("c".to_string()), mods);
        assert_eq!(hex(&emission.bytes), "03");
        assert!(
            !source.latch().armed(),
            "a transient modifier is never retained"
        );
        // …and the next unmodified key is untouched.
        let plain = source.on_key(&Key::Character("c".to_string()), Modifiers::default());
        assert_eq!(hex(&plain.bytes), "63");
    }

    #[test]
    fn a_hardware_shift_picks_the_shifted_sequence_for_its_own_event_only() {
        let mut source = KeystrokeSource::new();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };

        let tab = source.on_key(&Key::Named(NamedKey::Tab), shift);
        assert_eq!(hex(&tab.bytes), "1b5b5a", "Shift+Tab is back-tab");
        assert_eq!(tab.class, InputClass::Sequence);

        let up = source.on_key(&Key::Named(NamedKey::ArrowUp), shift);
        assert_eq!(hex(&up.bytes), "1b5b313b3241", "Shift+↑");

        // A held Shift is the event's own, never the bar's: nothing is retained
        // and the next unmodified key is the plain sequence again.
        assert!(!source.latch().armed());
        let plain = source.on_key(&Key::Named(NamedKey::ArrowUp), Modifiers::default());
        assert_eq!(hex(&plain.bytes), "1b5b41");

        // …and a shifted CHARACTER is still the character the platform cased.
        let typed = source.on_key(&Key::Character("A".to_string()), shift);
        assert_eq!(hex(&typed.bytes), "41");
    }

    #[test]
    fn a_named_key_sends_its_sequence_without_touching_the_mirror() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ab");
        let emission = source.on_key(&Key::Named(NamedKey::ArrowLeft), Modifiers::default());
        assert_eq!(hex(&emission.bytes), "1b5b44");
        assert_eq!(emission.class, InputClass::Sequence);
        assert_eq!(source.mirror(), "ab");
    }
}
