//! The keystroke source's **pure half**: the last-known-state diff that turns
//! platform IME snapshots back into terminal bytes, the one-shot Ctrl/Alt
//! latch every emission passes through, and the key bar's own byte vocabulary.
//!
//! No `Widget`, no `Rc`, no signals, no feed — everything here is a plain
//! value transformation, so the whole input model is unit-testable without a
//! render tree. [`widget`](super::widget) owns the IME clienthood
//! (`request_focus`/`publish_ime_state`) and
//! [`keybar`](crate::features::terminal::presentation::keybar) owns the bar's
//! chrome; both drive **this** type and send whatever bytes it hands back to
//! [`TerminalFeed::send_input`](crate::features::terminal::domain::feed::TerminalFeed::send_input).
//!
//! # Why a diff, and not a sentinel buffer
//!
//! Both mobile IME bridges are **state-sync, never op-forwarding** (frust's
//! `docs/CODE_STANDARDS.md`): the platform owns a text mirror and hands the app
//! a whole reconciled [`EditingState`], never a keystroke. A terminal needs the
//! opposite — one byte per key. The Frust benchmark probe `s10_keys` recovered
//! the keystroke by priming the mirror with a fixed **sentinel** (two
//! ZERO WIDTH SPACEs), diffing every snapshot against it and re-publishing the
//! sentinel after each one. That probe found the failure this module is built
//! to make impossible: an iOS smart-dash rewrite can rewrite the *prefix*, and
//! the invisible sentinel characters then leak into the byte stream as if the
//! user had typed them.
//!
//! So there is **no sentinel here, and no invented character anywhere**. The
//! source keeps the last snapshot it reconciled ([`KeystrokeSource::mirror`])
//! and diffs the next one against it in UTF-16 space:
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
//! # The mirror is a rolling window, re-anchored only on demand
//!
//! Re-publishing after every keystroke would be a `restartInput` per keystroke
//! on Android (`FrustSurfaceView.FrustInputConnection.reconcileTo` restarts
//! whenever the published text differs from the live `Editable`) — visible IME
//! churn. So the source publishes only when it **re-anchors** its window:
//!
//! | Trigger | Window becomes | Rate |
//! |---|---|---|
//! | grew past [`MIRROR_CAP`] | the last [`MIRROR_TAIL`] units | once per `CAP - TAIL` typed units |
//! | shrank below [`MIRROR_FLOOR`] | the last [`MIRROR_TAIL`] units of [`history`](KeystrokeSource) | once per `TAIL - FLOOR` deletes |
//!
//! A re-anchor is only sound because **both bridges apply the republished state
//! synchronously, inside the same round trip that delivered the keystroke**:
//! Android's `FrustInputConnection.sync()` calls `nativeImeApply` and then
//! immediately re-reads `nativeImeState` and reconciles its `Editable`, and
//! iOS's `syncToRust()` folds the returned state back into its
//! `NSMutableString` before returning. So the snapshot *after* a trim is
//! already built on the trimmed buffer — a bridge that applied it a frame later
//! would hand this engine a stale snapshot, which it would read as a large
//! edit. That premise is the one thing here that lives outside this app.
//!
//! The window is never re-anchored to *empty*: a deletion is only observable as
//! a **shortening**, so a platform mirror with nothing left in it silently
//! swallows every further backspace (Android's `deleteSurroundingText` on an
//! empty `Editable` is a no-op whose `sync()` then short-circuits; iOS's
//! `deleteBackward` returns without touching the mirror). Keeping a tail of the
//! user's own recently typed text — drawn from [`HISTORY_CAP`] units of
//! history, so a *trim* does not cost the user their backspace — is what makes
//! held-down backspace keep emitting. Once history itself is exhausted (the
//! user deleted everything this session ever saw) further backspaces are
//! genuinely invisible: a documented residual, not a papered-over one.
//!
//! # Rules, each pinned by a test
//!
//! - **Zero-composing is the norm.** A swipe-typed word arrives as ONE
//!   multi-character commit ⇒ one contiguous byte burst. If a composing range
//!   *does* appear it is **report-only**: nothing is emitted and the window is
//!   not moved, because re-publishing mid-composition destroys the platform's
//!   own composition state (s10's rule). [`KeystrokeSource::published`]
//!   therefore echoes the composing snapshot verbatim, so the paint-pass
//!   republish cannot fight the composition either.
//! - **Enter is asymmetric.** Android delivers `Key::Named(Enter)` (its
//!   `IME_ACTION_DONE`); iOS delivers a literal `"\n"` inside the snapshot.
//!   Both emit [`ENTER_BYTE`] (`0x0d`, CR) — what a terminal expects for
//!   Return, and what kterm sends. Every `\n` inside a committed run is
//!   translated the same way, so a pasted multi-line block is several Returns.
//! - **Backspace is deduped.** Android can deliver *both* a
//!   `Key::Named(Backspace)` and the matching snapshot shortening. The rule:
//!   **the key path emits and arms; the reconciler consumes the echo.** One
//!   `0x7f` reaches the session, not two — and a snapshot-only shortening
//!   (iOS, and Gboard's `deleteSurroundingText`) still emits exactly one. The
//!   arm is retired by the first snapshot that does *not* shorten, so a key
//!   event whose echo never arrives cannot swallow a later real delete.
//! - **Repeated identical characters each emit.** `"aaa"` is three appends of
//!   one unit, three bytes — the platform mirror no longer short-circuits a
//!   repeat (frust's Android bridge clears `lastPushed` on a wholesale
//!   replace), so nothing here has to work around one.
//! - **Nothing is transformed.** [`ImeContentType::Terminal`](frust::authoring::ImeContentType)
//!   suppresses smart quotes/dashes platform-side; this module additionally
//!   never rewrites a byte it was handed. `"` is `0x22` and `--` is `2d 2d`.
//!
//! # The latch is at the output choke point
//!
//! [`ModifierLatch`] transforms **soft-keyboard characters too** — that is the
//! incumbent's semantic, and the reason it lives on the emission path
//! ([`KeystrokeSource::emit`]) rather than in the key bar: arming `Ctrl` on the
//! bar and then typing `c` on the OS keyboard must send `0x03`. See
//! [`apply_latch`] for the transform itself.

use frust::authoring::{EditingState, Key, Modifiers, NamedKey};

// ---------------------------------------------------------------------------
// The byte vocabulary
// ---------------------------------------------------------------------------

/// The byte a derived Backspace sends: `0x7f` (DEL, `^?`).
///
/// **Not** `0x08` (BS), which the S10 probe used because nothing there fed a
/// real PTY. `0x7f` is what xterm and every `stty erase` default actually send,
/// and it is what the Flutter incumbent this app replaces sends.
pub const BACKSPACE_BYTE: u8 = 0x7f;

/// The byte a derived Return sends: `0x0d` (CR).
///
/// Both platforms' Return paths converge here (see the [module docs](self)):
/// a terminal expects CR for Return — the line discipline turns it into the
/// newline the program under it reads, and sending LF instead skips that.
pub const ENTER_BYTE: u8 = 0x0d;

/// `ESC` — the key bar's `Esc`, and the prefix of every sequence below.
pub const ESC: &[u8] = b"\x1b";
/// `HT` — the key bar's `Tab`.
pub const TAB: &[u8] = b"\t";
/// `CUU` — cursor up (the bar's `↑`).
pub const ARROW_UP: &[u8] = b"\x1b[A";
/// `CUD` — cursor down (the bar's `↓`).
pub const ARROW_DOWN: &[u8] = b"\x1b[B";
/// `CUF` — cursor forward (the bar's `→`).
pub const ARROW_RIGHT: &[u8] = b"\x1b[C";
/// `CUB` — cursor back (the bar's `←`).
pub const ARROW_LEFT: &[u8] = b"\x1b[D";
/// Page up (the bar's `PgUp`).
pub const PAGE_UP: &[u8] = b"\x1b[5~";
/// Page down (the bar's `PgDn`).
pub const PAGE_DOWN: &[u8] = b"\x1b[6~";
/// Forward delete (the bar's `Del`) — **not** Backspace.
pub const FORWARD_DELETE: &[u8] = b"\x1b[3~";
/// Home, in the normal (non-application) cursor mode a fresh session is in.
pub const HOME: &[u8] = b"\x1b[H";
/// End, same mode as [`HOME`].
pub const END: &[u8] = b"\x1b[F";

// ---------------------------------------------------------------------------
// The rolling window
// ---------------------------------------------------------------------------

/// How many UTF-16 code units the published mirror may reach before the source
/// re-anchors it (see the [module docs](self)).
pub const MIRROR_CAP: usize = 96;

/// How many units a re-anchor leaves in the mirror. The gap to [`MIRROR_CAP`]
/// is how many characters may be typed between two re-anchors, and therefore
/// how rare a platform `restartInput` is.
pub const MIRROR_TAIL: usize = 24;

/// How short the mirror may get before a re-anchor refills it from history —
/// never zero, because a delete is only observable as a *shortening* (see the
/// [module docs](self)).
pub const MIRROR_FLOOR: usize = 4;

/// How many units of the user's own recent typing the source retains to refill
/// the mirror from. Bounds the source's memory; past it, the oldest typing is
/// forgotten and can no longer be backspaced into.
pub const HISTORY_CAP: usize = 256;

/// How many un-echoed `Key::Named(Backspace)`s may be armed at once (see the
/// dedupe rule in the [module docs](self)). A ceiling, not a queue: it bounds
/// what a platform that only ever sends the key event (and never the echo)
/// could suppress.
pub const MAX_PENDING_BACKSPACES: usize = 4;

// ---------------------------------------------------------------------------
// UTF-16 arithmetic (the seam's index unit)
// ---------------------------------------------------------------------------

/// `s`'s length in **UTF-16 code units** — the unit every index at the IME seam
/// is measured in (frust's `docs/CODE_STANDARDS.md`: UTF-16 at the FFI seam,
/// bytes inside).
///
/// Deliberately never `s.len()`: `"\u{1f600}"` is 4 bytes, 1 char and 2 units,
/// and a byte-length diff is the first bug this pattern invites.
pub fn len_utf16(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The last `units` UTF-16 code units of `s`, rounded **outward** to a
/// character boundary so a surrogate pair is never split (asking for one unit
/// of a trailing emoji yields the whole emoji).
pub fn utf16_suffix(s: &str, units: usize) -> &str {
    if units == 0 {
        return "";
    }
    let mut taken = 0usize;
    let mut start = s.len();
    for (offset, ch) in s.char_indices().rev() {
        if taken >= units {
            break;
        }
        taken += ch.len_utf16();
        start = offset;
    }
    &s[start..]
}

/// Drop leading characters from `s` until it is at most `units` UTF-16 code
/// units long, character-aligned — [`utf16_suffix`] as an in-place truncation.
fn clamp_front(s: &mut String, units: usize) {
    if len_utf16(s) <= units {
        return;
    }
    let keep = utf16_suffix(s, units).to_string();
    *s = keep;
}

/// The minimal `(removed, added)` edit turning `old` into `new`: the common
/// prefix and common suffix are cancelled, `removed` counts the UTF-16 units
/// the middle of `old` held, and `added` is the middle of `new`.
///
/// Both boundaries are **character**-aligned (Rust `char`s are whole scalars),
/// so a surrogate pair can never be split across a diff boundary — the emoji
/// case that a naive UTF-16-indexed diff gets wrong.
pub fn diff(old: &str, new: &str) -> (usize, String) {
    let mut prefix = 0usize;
    for (a, b) in old.chars().zip(new.chars()) {
        if a != b {
            break;
        }
        prefix += a.len_utf8();
    }

    let mut suffix = 0usize;
    let (old_rest, new_rest) = (&old[prefix..], &new[prefix..]);
    let mut old_back = old_rest.chars().rev();
    let mut new_back = new_rest.chars().rev();
    loop {
        match (old_back.next(), new_back.next()) {
            (Some(a), Some(b)) if a == b => suffix += a.len_utf8(),
            _ => break,
        }
    }

    let removed = &old[prefix..old.len() - suffix];
    let added = &new[prefix..new.len() - suffix];
    (len_utf16(removed), added.to_string())
}

/// The bytes a committed run of text sends: its UTF-8, with every `\n`
/// translated to [`ENTER_BYTE`].
///
/// The **only** rewrite this module performs, and it is the Return contract,
/// not a "correction" (see the [module docs](self)). A pasted `"\r\n"`
/// therefore sends two CRs — two Returns — which is the same thing every
/// naive paste into a terminal does.
pub fn text_bytes(text: &str) -> Vec<u8> {
    text.bytes()
        .map(|b| if b == b'\n' { ENTER_BYTE } else { b })
        .collect()
}

// ---------------------------------------------------------------------------
// The one-shot Ctrl/Alt latch
// ---------------------------------------------------------------------------

/// The armed one-shot modifiers, as toggled from the key bar (and as carried by
/// a desktop hardware key event).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifierLatch {
    /// `Ctrl` is armed for the next single-code-unit emission.
    pub ctrl: bool,
    /// `Alt` is armed for the next single-code-unit emission.
    pub alt: bool,
}

impl ModifierLatch {
    /// Nothing armed.
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
    };

    /// Whether either flag is armed.
    pub fn armed(self) -> bool {
        self.ctrl || self.alt
    }

    /// Both flags of either side (used to fold a hardware key event's own
    /// modifiers into the latch for that one emission).
    pub fn union(self, other: Self) -> Self {
        Self {
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
        }
    }

    /// The latch a desktop hardware key event carries — `Modifiers`' ctrl/alt,
    /// applied once to that event and never retained.
    pub fn from_modifiers(mods: Modifiers) -> Self {
        Self {
            ctrl: mods.ctrl,
            alt: mods.alt,
        }
    }
}

/// Whether `data` is exactly **one UTF-16 code unit**, and if so its value.
///
/// The latch's eligibility test: `'a'` (1 byte) and `'é'` (2 bytes) are both one
/// unit and both transformable; an emoji (2 units) and `\x1b[A` (3 units) are
/// not. Invalid UTF-8 is never eligible.
pub fn single_unit(data: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(data).ok()?;
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || ch.len_utf16() != 1 {
        return None;
    }
    Some(ch as u32)
}

/// Apply `latch` to one outgoing emission.
///
/// The rules, verbatim from the incumbent:
///
/// 1. **Only a single-code-unit emission is transformed.** Anything longer (an
///    arrow sequence, a swipe-typed word, a backspace run) passes through
///    **unmodified** — and, at the call site, leaves the latch armed.
/// 2. **Ctrl** uppercases a lowercase ASCII letter (`0x61..=0x7a` → `−0x20`)
///    and then masks `& 0x1f` **only if** the (uppercased) code lands in
///    `0x40..=0x5f`. Outside that window there is no Ctrl transform at all —
///    `Ctrl+2` sends `'2'` — though Alt may still apply.
/// 3. **Alt** prefixes [`ESC`], the `ESC`-prefix convention every mainstream
///    terminal uses for Meta.
pub fn apply_latch(latch: ModifierLatch, data: &[u8]) -> Vec<u8> {
    let Some(unit) = single_unit(data) else {
        return data.to_vec();
    };
    if !latch.armed() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len() + 1);
    if latch.alt {
        out.extend_from_slice(ESC);
    }
    if latch.ctrl {
        let upper = if (0x61..=0x7a).contains(&unit) {
            unit - 0x20
        } else {
            unit
        };
        if (0x40..=0x5f).contains(&upper) {
            out.push((upper & 0x1f) as u8);
            return out;
        }
    }
    out.extend_from_slice(data);
    out
}

// ---------------------------------------------------------------------------
// The key bar's keys
// ---------------------------------------------------------------------------

/// One key of the terminal key bar — the eleven keys a soft keyboard has no
/// room for, in the order the incumbent draws them.
///
/// [`Ctrl`](Self::Ctrl)/[`Alt`](Self::Alt) send nothing: they toggle the
/// [`ModifierLatch`] the *next* emission passes through, wherever that emission
/// comes from (see the [module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKey {
    /// Arms/disarms the one-shot `Ctrl`.
    Ctrl,
    /// Arms/disarms the one-shot `Alt`.
    Alt,
    /// `ESC`.
    Esc,
    /// `HT`.
    Tab,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Forward delete — the one destructive key, and the one drawn in the
    /// danger tone.
    Del,
}

impl BarKey {
    /// Every key, in bar order: the two latches, `Esc`, │ `Tab` and the four
    /// arrows, │ the two page keys, │ `Del`.
    pub const ORDER: [BarKey; 11] = [
        BarKey::Ctrl,
        BarKey::Alt,
        BarKey::Esc,
        BarKey::Tab,
        BarKey::Up,
        BarKey::Down,
        BarKey::Left,
        BarKey::Right,
        BarKey::PageUp,
        BarKey::PageDown,
        BarKey::Del,
    ];

    /// The cap's printed label.
    pub fn label(self) -> &'static str {
        match self {
            BarKey::Ctrl => "Ctrl",
            BarKey::Alt => "Alt",
            BarKey::Esc => "Esc",
            BarKey::Tab => "Tab",
            BarKey::Up => "\u{2191}",
            BarKey::Down => "\u{2193}",
            BarKey::Left => "\u{2190}",
            BarKey::Right => "\u{2192}",
            BarKey::PageUp => "PgUp",
            BarKey::PageDown => "PgDn",
            BarKey::Del => "Del",
        }
    }

    /// The cap's accessible name — a **word**, never the arrow glyph it draws
    /// (`crate::ui::topbar`'s `BACK_LABEL` rule).
    pub fn name(self) -> &'static str {
        match self {
            BarKey::Ctrl => "Control",
            BarKey::Alt => "Alt",
            BarKey::Esc => "Escape",
            BarKey::Tab => "Tab",
            BarKey::Up => "Up arrow",
            BarKey::Down => "Down arrow",
            BarKey::Left => "Left arrow",
            BarKey::Right => "Right arrow",
            BarKey::PageUp => "Page up",
            BarKey::PageDown => "Page down",
            BarKey::Del => "Delete",
        }
    }

    /// The bytes this key sends, or `None` for the two latch keys.
    pub fn sequence(self) -> Option<&'static [u8]> {
        match self {
            BarKey::Ctrl | BarKey::Alt => None,
            BarKey::Esc => Some(ESC),
            BarKey::Tab => Some(TAB),
            BarKey::Up => Some(ARROW_UP),
            BarKey::Down => Some(ARROW_DOWN),
            BarKey::Left => Some(ARROW_LEFT),
            BarKey::Right => Some(ARROW_RIGHT),
            BarKey::PageUp => Some(PAGE_UP),
            BarKey::PageDown => Some(PAGE_DOWN),
            BarKey::Del => Some(FORWARD_DELETE),
        }
    }

    /// Whether this key toggles the latch rather than sending anything.
    pub fn is_latch(self) -> bool {
        self.sequence().is_none()
    }

    /// Whether this key is drawn in the danger tone (`Del` alone).
    pub fn is_danger(self) -> bool {
        matches!(self, BarKey::Del)
    }

    /// Whether a group separator is drawn immediately **before** this key —
    /// the three rules of the incumbent's bar (`Ctrl Alt Esc │ Tab ← ↑ ↓ → │
    /// PgUp PgDn │ Del`).
    pub fn separator_before(self) -> bool {
        matches!(self, BarKey::Tab | BarKey::PageUp | BarKey::Del)
    }

    /// Whether this key is currently armed, given `latch`. Only the two latch
    /// keys can be.
    pub fn armed(self, latch: ModifierLatch) -> bool {
        match self {
            BarKey::Ctrl => latch.ctrl,
            BarKey::Alt => latch.alt,
            _ => false,
        }
    }
}

/// The bytes a focus-routed [`NamedKey`] sends — the desktop preview's real
/// keyboard, and Android's `IME_ACTION_DONE`-as-`Enter`.
///
/// An exhaustive match, so a new [`NamedKey`] variant is a compile error here
/// rather than a silently dropped key. Shares its sequences with [`BarKey`]:
/// the bar's `↑` and a physical `↑` send the same three bytes.
pub fn named_key_bytes(key: NamedKey) -> &'static [u8] {
    match key {
        NamedKey::Enter => &[ENTER_BYTE],
        NamedKey::Backspace => &[BACKSPACE_BYTE],
        NamedKey::Delete => FORWARD_DELETE,
        NamedKey::ArrowUp => ARROW_UP,
        NamedKey::ArrowDown => ARROW_DOWN,
        NamedKey::ArrowLeft => ARROW_LEFT,
        NamedKey::ArrowRight => ARROW_RIGHT,
        NamedKey::Home => HOME,
        NamedKey::End => END,
        NamedKey::Escape => ESC,
        NamedKey::Tab => TAB,
    }
}

// ---------------------------------------------------------------------------
// What one input event derived
// ---------------------------------------------------------------------------

/// What the source concluded about one event — diagnostics, and what the tests
/// assert the classification on.
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
}

/// One event's outcome: the bytes to send, and whether the platform mirror has
/// to be re-published.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Emission {
    /// The bytes to hand to
    /// [`TerminalFeed::send_input`](crate::features::terminal::domain::feed::TerminalFeed::send_input).
    /// Empty is the common case for a bracket or an echo.
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

    /// Whether anything at all happened (bytes, a re-anchor, or a latch
    /// change) — what the caller gates its redraw/wake on.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty() && !self.reconciled && self.class == InputClass::None
    }
}

// ---------------------------------------------------------------------------
// The source itself
// ---------------------------------------------------------------------------

/// The keystroke source: the platform-mirror baseline, the modifier latch, and
/// the counters the tests read.
///
/// One instance is shared (behind an `Rc<RefCell<_>>` — see
/// [`SharedKeystrokeSource`](super::SharedKeystrokeSource)) between the grid
/// widget and the key bar, so an emission from either passes through the same
/// latch.
pub struct KeystrokeSource {
    /// What the platform mirror is believed to hold — the diff baseline.
    /// Always a suffix of [`history`](Self::history).
    mirror: String,
    /// The user's own recent typing, [`HISTORY_CAP`] units of it. A re-anchor
    /// refills the mirror from here; nothing else reads it.
    history: String,
    /// The live composing snapshot, while one exists — echoed verbatim by
    /// [`published`](Self::published) so the paint-pass republish cannot
    /// destroy the platform's composition.
    composing: Option<EditingState>,
    /// Armed by a `Key::Named(Backspace)`, consumed by the matching snapshot
    /// shortening (the dedupe rule).
    pending_backspaces: usize,
    /// The one-shot Ctrl/Alt latch.
    latch: ModifierLatch,
    /// Bumped on every state change — mirrored into the seam's signal so the
    /// key bar's armed pips rebuild.
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
    pub fn new() -> Self {
        Self {
            mirror: String::new(),
            history: String::new(),
            composing: None,
            pending_backspaces: 0,
            latch: ModifierLatch::NONE,
            revision: 0,
            reconciles: 0,
            emitted: 0,
        }
    }

    /// The editing state to publish right now.
    ///
    /// The mirror with the caret collapsed at its end — **except** while a
    /// composition is live, where it is the platform's own snapshot echoed
    /// back, so re-publishing it is a no-op on both bridges instead of a
    /// composition-destroying wholesale replace.
    pub fn published(&self) -> EditingState {
        if let Some(state) = &self.composing {
            return state.clone();
        }
        let end = i32::try_from(len_utf16(&self.mirror)).unwrap_or(i32::MAX);
        EditingState {
            text: self.mirror.clone(),
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

    /// The mirror's current text — a test/diagnostic accessor.
    pub fn mirror(&self) -> &str {
        &self.mirror
    }

    /// How many times the window has been re-anchored — the publish counter the
    /// trim test caps.
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

    /// Whether a composition is live (report-only, see the [module docs](self)).
    pub fn is_composing(&self) -> bool {
        self.composing.is_some()
    }

    // -- the emission choke point -------------------------------------------

    /// **The one place bytes are produced.** Applies the latch (plus any
    /// transient hardware modifiers) and, on a single-code-unit emission,
    /// disarms *both* flags together.
    ///
    /// A longer emission — an arrow sequence, a swipe-typed word, a run of
    /// backspaces — passes through untransformed and leaves the latch armed, so
    /// `Ctrl` then `↑` then `c` still sends `0x03`.
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

    // -- the mirror window ---------------------------------------------------

    /// Accept `next` as the platform mirror, folding it into history (whose
    /// tail *is* the old mirror, by invariant).
    fn set_mirror(&mut self, next: String) {
        let head = self.history.len() - self.mirror.len();
        self.history.truncate(head);
        self.history.push_str(&next);
        clamp_front(&mut self.history, HISTORY_CAP);
        self.mirror = next;
    }

    /// Re-anchor the window if it has grown past [`MIRROR_CAP`] or shrunk below
    /// [`MIRROR_FLOOR`], reporting whether the caller must re-publish.
    ///
    /// The shrink case only fires when history actually has more to give: with
    /// nothing left to refill from, re-anchoring every snapshot would publish
    /// forever and still not make a delete observable.
    fn reanchor_if_needed(&mut self) -> bool {
        let len = len_utf16(&self.mirror);
        let over = len > MIRROR_CAP;
        let under = len < MIRROR_FLOOR;
        if !over && !under {
            return false;
        }
        let target = utf16_suffix(&self.history, MIRROR_TAIL);
        if !over && len_utf16(target) <= len {
            return false;
        }
        self.mirror = target.to_string();
        self.reconciles += 1;
        self.revision += 1;
        true
    }

    // -- the platform paths --------------------------------------------------

    /// Reconcile one mobile snapshot ([`ImeEvent::ApplyEditingState`](frust::authoring::ImeEvent))
    /// against the mirror — **the diff engine**.
    ///
    /// Order matters, and this is the order:
    ///
    /// 1. **A live composing range wins**: report, emit nothing, move nothing.
    /// 2. Diff against the mirror; a shortening consumes any armed
    ///    `Key::Named(Backspace)` echo first (the dedupe rule), and any snapshot
    ///    that does not shorten retires the arm.
    /// 3. Emit the backspaces, then the added text — each through the latch.
    /// 4. Accept the snapshot as the new mirror, and re-anchor if the window
    ///    has drifted out of `FLOOR..=CAP`.
    pub fn on_snapshot(&mut self, state: &EditingState) -> Emission {
        if state.composing_base >= 0 && state.composing_extent > state.composing_base {
            // Report-only: re-publishing mid-composition would destroy the
            // platform's own composition state (see the module docs).
            self.composing = Some(state.clone());
            self.revision += 1;
            return Emission::inert(InputClass::Composing);
        }
        self.composing = None;

        let (removed, added) = diff(&self.mirror, &state.text);
        let mut backspaces = removed;
        if backspaces > 0 {
            let consumed = backspaces.min(self.pending_backspaces);
            backspaces -= consumed;
            self.pending_backspaces -= consumed;
        } else {
            // The echo never came (or this was an append): retire the arm so it
            // cannot swallow a later real delete.
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
            (_, "\n") | (_, "\r") | (_, "\r\n") => InputClass::Enter,
            (_, "") if removed > 0 => InputClass::Backspace,
            (_, "") => InputClass::None,
            _ => InputClass::Text,
        };

        self.set_mirror(state.text.clone());
        let reconciled = self.reanchor_if_needed();
        Emission {
            bytes,
            reconciled,
            class,
        }
    }

    /// A desktop (winit) commit — the composed text the platform finished.
    ///
    /// Emitted straight through: there is no platform mirror to diff against on
    /// desktop, so the mirror is left alone.
    pub fn on_commit(&mut self, text: &str) -> Emission {
        self.composing = None;
        if text.is_empty() {
            return Emission::inert(InputClass::None);
        }
        let class = if text == "\n" || text == "\r" || text == "\r\n" {
            InputClass::Enter
        } else {
            InputClass::Text
        };
        let bytes = self.emit(&text_bytes(text), ModifierLatch::NONE);
        Emission {
            bytes,
            reconciled: false,
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
        self.composing = Some(EditingState {
            text: text.to_string(),
            selection_base: units,
            selection_extent: units,
            composing_base: 0,
            composing_extent: units,
        });
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

    /// A focus-routed key event: Android's `IME_ACTION_DONE`-as-`Enter`, and
    /// every real key on the desktop preview.
    ///
    /// A `Key::Named(Backspace)` **arms the dedupe** (see the module docs): it
    /// emits its byte here and the reconciler swallows the platform's echo of
    /// the same delete.
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
                let bytes = self.emit(named_key_bytes(*named), transient);
                Emission {
                    bytes,
                    reconciled: false,
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
                Emission {
                    bytes,
                    reconciled: false,
                    class,
                }
            }
        }
    }

    /// A key-bar tap: a latch toggle, or a control sequence sent through the
    /// same choke point every other emission uses.
    pub fn on_bar_key(&mut self, key: BarKey) -> Emission {
        match key.sequence() {
            None => {
                match key {
                    BarKey::Ctrl => self.latch.ctrl = !self.latch.ctrl,
                    BarKey::Alt => self.latch.alt = !self.latch.alt,
                    // Unreachable: only the two latch keys have no sequence.
                    _ => {}
                }
                self.revision += 1;
                Emission::inert(InputClass::Latch)
            }
            Some(sequence) => {
                let bytes = self.emit(sequence, ModifierLatch::NONE);
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
    use super::*;

    /// The ordinary mobile snapshot shape: caret collapsed at the end, no
    /// composing region.
    fn snapshot(text: &str) -> EditingState {
        let end = i32::try_from(len_utf16(text)).unwrap_or(i32::MAX);
        EditingState {
            text: text.to_string(),
            selection_base: end,
            selection_extent: end,
            composing_base: -1,
            composing_extent: -1,
        }
    }

    /// Lowercase hex pairs — how every byte assertion below reads.
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Type `text` one character at a time, exactly as a platform mirror would:
    /// each snapshot is **the mirror the source currently publishes** plus one
    /// character.
    ///
    /// Re-reading the mirror per keystroke is not a shortcut — it is the
    /// contract. Both bridges apply a re-anchor synchronously inside the same
    /// round trip that delivered the keystroke (see the module docs), so the
    /// snapshot after a trim is built on the *trimmed* buffer.
    fn type_text(source: &mut KeystrokeSource, text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for ch in text.chars() {
            let mut buffer = source.mirror().to_string();
            buffer.push(ch);
            out.extend(source.on_snapshot(&snapshot(&buffer)).bytes);
        }
        out
    }

    // -----------------------------------------------------------------
    // UTF-16 arithmetic
    // -----------------------------------------------------------------

    #[test]
    fn len_utf16_counts_units_not_bytes_or_chars() {
        assert_eq!(len_utf16(""), 0);
        assert_eq!(len_utf16("abc"), 3);
        assert_eq!(len_utf16("\u{e9}"), 1, "2 bytes, 1 unit");
        assert_eq!(len_utf16("\u{1f600}"), 2, "4 bytes, 1 char, 2 units");
    }

    #[test]
    fn utf16_suffix_rounds_outward_and_never_splits_a_pair() {
        assert_eq!(utf16_suffix("abcdef", 3), "def");
        assert_eq!(utf16_suffix("abc", 9), "abc");
        assert_eq!(utf16_suffix("abc", 0), "");
        // One unit of a trailing surrogate pair yields the whole character.
        assert_eq!(utf16_suffix("ab\u{1f600}", 1), "\u{1f600}");
        assert_eq!(utf16_suffix("ab\u{1f600}", 2), "\u{1f600}");
        assert_eq!(utf16_suffix("ab\u{1f600}", 3), "b\u{1f600}");
    }

    #[test]
    fn diff_cancels_the_common_prefix_and_suffix() {
        assert_eq!(diff("", "a"), (0, "a".to_string()));
        assert_eq!(diff("ab", "abc"), (0, "c".to_string()));
        assert_eq!(diff("abc", "ab"), (1, String::new()));
        assert_eq!(diff("abc", "axc"), (1, "x".to_string()));
        assert_eq!(diff("abc", "abc"), (0, String::new()));
        // A deleted emoji costs two units, and the boundary stays on a char.
        assert_eq!(diff("a\u{1f600}", "a"), (2, String::new()));
    }

    // -----------------------------------------------------------------
    // The latch (table-driven)
    // -----------------------------------------------------------------

    #[test]
    fn the_latch_table_matches_the_incumbent_verbatim() {
        let ctrl = ModifierLatch {
            ctrl: true,
            alt: false,
        };
        let alt = ModifierLatch {
            ctrl: false,
            alt: true,
        };
        let both = ModifierLatch {
            ctrl: true,
            alt: true,
        };
        let cases: [(ModifierLatch, &[u8], &str, &str); 9] = [
            (ctrl, b"c", "03", "Ctrl+c is ETX"),
            (
                ctrl,
                b"C",
                "03",
                "an already-uppercase letter masks the same",
            ),
            (ctrl, b"[", "1b", "0x5b is inside the window"),
            (ctrl, b"2", "32", "0x32 is outside: no Ctrl transform"),
            (alt, b"c", "1b63", "Alt prefixes ESC"),
            (both, b"a", "1b01", "Alt's ESC, then Ctrl's masked byte"),
            (both, b"2", "1b32", "outside the window, Alt still applies"),
            (
                ctrl,
                ARROW_UP,
                "1b5b41",
                "a multi-unit sequence passes through",
            ),
            (
                ctrl,
                "\u{1f600}".as_bytes(),
                "f09f9880",
                "a surrogate pair is two units: untouched",
            ),
        ];
        for (latch, data, expected, why) in cases {
            assert_eq!(hex(&apply_latch(latch, data)), expected, "{why}");
        }
    }

    #[test]
    fn a_single_unit_emission_disarms_both_flags_and_a_longer_one_does_not() {
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Ctrl);
        source.on_bar_key(BarKey::Alt);
        assert_eq!(
            source.latch(),
            ModifierLatch {
                ctrl: true,
                alt: true
            }
        );

        // An arrow is three units: passes through AND leaves the latch armed.
        let arrow = source.on_bar_key(BarKey::Up);
        assert_eq!(hex(&arrow.bytes), "1b5b41");
        assert!(source.latch().armed(), "arrows leave the latch armed");

        // …so the next single-unit emission still sees both flags.
        let typed = type_text(&mut source, "a");
        assert_eq!(hex(&typed), "1b01", "Ctrl+Alt+a");
        assert_eq!(
            source.latch(),
            ModifierLatch::NONE,
            "both flags clear together"
        );
    }

    #[test]
    fn ctrl_outside_the_window_still_consumes_the_latch() {
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Ctrl);
        assert_eq!(hex(&type_text(&mut source, "2")), "32", "'2' is unchanged");
        assert!(!source.latch().armed(), "…but the latch is consumed");
    }

    #[test]
    fn the_latch_transforms_a_soft_keyboard_character_too() {
        // The whole reason the latch lives on the emission path: `Ctrl` armed
        // from the bar, `c` typed on the OS keyboard.
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Ctrl);
        assert_eq!(hex(&type_text(&mut source, "c")), "03");
    }

    // -----------------------------------------------------------------
    // The bar's own vocabulary
    // -----------------------------------------------------------------

    #[test]
    fn every_bar_key_sends_the_documented_sequence() {
        let expected: [(BarKey, &str); 9] = [
            (BarKey::Esc, "1b"),
            (BarKey::Tab, "09"),
            (BarKey::Up, "1b5b41"),
            (BarKey::Down, "1b5b42"),
            (BarKey::Left, "1b5b44"),
            (BarKey::Right, "1b5b43"),
            (BarKey::PageUp, "1b5b357e"),
            (BarKey::PageDown, "1b5b367e"),
            (BarKey::Del, "1b5b337e"),
        ];
        for (key, hex_bytes) in expected {
            let mut source = KeystrokeSource::new();
            assert_eq!(hex(&source.on_bar_key(key).bytes), hex_bytes, "{key:?}");
        }
        assert!(BarKey::Ctrl.sequence().is_none());
        assert!(BarKey::Alt.sequence().is_none());
    }

    #[test]
    fn the_bar_order_and_grouping_match_the_incumbent() {
        assert_eq!(
            BarKey::ORDER.map(BarKey::label),
            [
                "Ctrl", "Alt", "Esc", "Tab", "\u{2191}", "\u{2193}", "\u{2190}", "\u{2192}",
                "PgUp", "PgDn", "Del"
            ]
        );
        let separators: Vec<&str> = BarKey::ORDER
            .iter()
            .filter(|key| key.separator_before())
            .map(|key| key.label())
            .collect();
        assert_eq!(separators, ["Tab", "PgUp", "Del"], "three group rules");
        assert!(BarKey::Del.is_danger());
        assert!(!BarKey::Esc.is_danger());
    }

    #[test]
    fn a_named_key_and_its_bar_twin_send_the_same_bytes() {
        assert_eq!(named_key_bytes(NamedKey::ArrowUp), ARROW_UP);
        assert_eq!(named_key_bytes(NamedKey::Escape), ESC);
        assert_eq!(named_key_bytes(NamedKey::Tab), TAB);
        assert_eq!(named_key_bytes(NamedKey::Delete), FORWARD_DELETE);
        assert_eq!(named_key_bytes(NamedKey::Enter), &[ENTER_BYTE]);
        assert_eq!(named_key_bytes(NamedKey::Backspace), &[BACKSPACE_BYTE]);
    }

    // -----------------------------------------------------------------
    // The diff engine
    // -----------------------------------------------------------------

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
    }

    #[test]
    fn android_delivers_return_as_a_key_event_and_it_emits_the_same_cr() {
        let mut source = KeystrokeSource::new();
        let emission = source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert_eq!(hex(&emission.bytes), "0d");
        assert_eq!(emission.class, InputClass::Enter);
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
    fn an_unchanged_snapshot_is_our_own_echo_and_emits_nothing() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ab");
        let emission = source.on_snapshot(&snapshot("ab"));
        assert!(emission.bytes.is_empty());
        assert_eq!(emission.class, InputClass::None);
        assert!(emission.is_empty());
    }

    // -----------------------------------------------------------------
    // The rolling window
    // -----------------------------------------------------------------

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

    // -----------------------------------------------------------------
    // Desktop paths
    // -----------------------------------------------------------------

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
    fn a_named_key_sends_its_sequence_without_touching_the_mirror() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ab");
        let emission = source.on_key(&Key::Named(NamedKey::ArrowLeft), Modifiers::default());
        assert_eq!(hex(&emission.bytes), "1b5b44");
        assert_eq!(emission.class, InputClass::Sequence);
        assert_eq!(source.mirror(), "ab");
    }
}
