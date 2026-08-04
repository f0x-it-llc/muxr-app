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
//! # What is retained, and when it is scrubbed
//!
//! Everything in [`mirror`](KeystrokeSource) and
//! [`history`](KeystrokeSource) is, by construction, **text the user typed**
//! — which on a terminal includes the `sudo` password, the ssh passphrase and
//! the pasted API token. Remote echo-off does not help: the IME reports the
//! keystroke to the app either way, because the app *is* the keyboard's
//! client. So (review M6):
//!
//! - both buffers are [`Zeroizing<String>`](zeroize::Zeroizing) — a drop or an
//!   overwrite scrubs the bytes instead of leaving them in a freed allocation,
//!   the same treatment `ConnectController::token` gives the login token;
//! - so is the composing echo's text (`ComposingEcho`, review FC2): a
//!   composing snapshot carries the **whole** platform mirror, so parking it
//!   as a plain `EditingState` was the same finding one indirection out;
//! - the paths that used to allocate a fresh plaintext copy per keystroke
//!   (`clamp_front`, `set_mirror`, the re-anchor, the composing echo) now
//!   rewrite the buffers **in place**, and the one temporary that survives —
//!   the diff's `added` middle — is wrapped so its own drop scrubs it;
//! - nothing oversized is ever *stored* in **either diff buffer**:
//!   `set_mirror` keeps only the clamped suffix of a snapshot, so a pasted
//!   token or a dictation burst cannot grow the mirror or the history past the
//!   capacity [`new`](KeystrokeSource::new) pre-reserved (a reallocation frees
//!   the old plaintext where no scrub can reach it — `zeroize`'s own
//!   documented caveat). The **composing echo is deliberately unbounded**, and
//!   is the one buffer that is: a composition's indices are indices into it,
//!   so clamping it would hand the bridge a range into text that cannot hold
//!   it. Growing it is safe because it — like the mirror — is only ever
//!   written `zeroize()`-then-`push_str`, so the allocation a growth frees
//!   holds zeros rather than the previous composition. The **history** is the
//!   one buffer that appends onto live plaintext, which is why its reserve,
//!   and only its reserve, has to cover the outward-rounded bound (review FC3;
//!   see [`new`](KeystrokeSource::new));
//! - [`KeystrokeSource::scrub`] drops both buffers, the composing echo, and
//!   the diff baseline with them, at every boundary where the text stops being
//!   live: a submitted line (below), a blur, `read_only`, and the pane's
//!   teardown (see [`InputSeam`](super::InputSeam) for the wiring of the last
//!   three);
//! - the **one** thing a scrub keeps about what it dropped is a keyed digest
//!   of it — a `u64` under this instance's own random seed, plus a UTF-16
//!   length, and never a character of the text (review R2-F2). The stale-replay
//!   guard below needs to recognize that exact line coming back; a seeded
//!   64-bit digest is what lets it, while being meaningless in another source
//!   or another process. It is dropped with the one shot it belongs to.
//!
//! **The residuals, recorded rather than chased** (deliberately unnumbered:
//! the list grows):
//!
//! - the platform's own mirror is plaintext in the platform's own process —
//!   nothing here can reach it (the app-side half of the same finding
//!   `docs/ARCHITECTURE.md` records for the login field);
//! - [`published`](KeystrokeSource::published) hands the framework a plain
//!   `EditingState`, so the units currently being typed are copied once per
//!   focused frame into a buffer this module does not own; the scrub
//!   boundaries are what keep that window short;
//! - the emitted bytes themselves — [`text_bytes`] → the latch →
//!   [`Emission::bytes`] → `send_input` — are a plain `Vec<u8>` of the user's
//!   own keystrokes, which is inherent to *sending* them and ends where the
//!   feed's own write does;
//! - a key cap holding the focus path when the keyboard is dismissed keeps the
//!   buffers until the pane's teardown (already recorded on the grid widget's
//!   `note_focus_edge`; closing it needs a key-cap teardown scrub, a separate
//!   item and not this module's);
//! - the post-scrub replay guard below compares **content** as well as shape
//!   (review R2-F2), so a word-shaped stale line — `htop`, a word-shaped
//!   password — no longer replays. What it cannot see is a submitted line of
//!   at most one keystroke's worth (`ls`, one emoji): no digest is recorded at
//!   that length, deliberately, so such a line still replays in the frames the
//!   platform is behind — one keystroke of bytes, which nothing here can tell
//!   from the same key pressed again. The guard's precise closure list is in
//!   its own section below.
//!
//! # A submitted line is dropped, not kept as a baseline
//!
//! Every emission carrying a Return re-anchors the window to **empty**
//! (review M7): the mirror and the history both go, so no `\n` is ever left in
//! either. Two reasons, and the security one is the load-bearing one:
//!
//! 1. iOS accumulates the literal `\n` in its mirror, and
//!    [`text_bytes`] maps `\n` to CR — so *any* bulk re-emission of a buffer
//!    holding one (see the desync guards below) would re-execute previously
//!    typed commands on the remote host. Dropping the baseline makes that
//!    impossible **from this module's own buffers** — precisely, not
//!    "structurally impossible" full stop: the *platform's* buffer is a
//!    second one, and closing it is the next section.
//! 2. A submitted line needs no baseline: the shell has taken it, and the next
//!    keystroke belongs to a fresh line.
//!
//! The cost is one platform `restartInput` per Return — the rarest keystroke —
//! and one accepted regression: a Backspace pressed **immediately** after a
//! Return has no shortening to be observed as (the mirror is empty), so it
//! emits nothing until one character has been typed. That is the same
//! documented residual as an exhausted history, and at a shell prompt a
//! Backspace on an empty line is a no-op anyway.
//!
//! ## The frame the platform may still be holding the old line
//!
//! Dropping *our* baseline does not drop the *platform's*. An ordinary edit
//! round trip is applied synchronously (above), but a **submit** is not:
//! Android's `FrustInputConnection.performEditorAction` forwards
//! `IME_ACTION_DONE` through `nativeImeAction` and then only counts down
//! `imeResyncFrames`, its own comment recording that "a submit clears the
//! field on the next-frame rebuild". For those frames the `Editable` still
//! holds the pre-submit text; on iOS a round trip that errors, answers nil or
//! is simply a frame late leaves the same window open.
//!
//! Diffed against the empty baseline the scrub left, **that** snapshot reads
//! as one `added` run holding the whole submitted line. Emitted, it re-executes
//! the command on a remote host that is back at a prompt — or re-transmits the
//! password typed at a prompt with echo *off* into a shell with echo *on*. The
//! oversized-*delete* guard below cannot see this one: nothing was removed
//! (review FC2).
//!
//! And the stale line does not necessarily carry a Return — which is the half
//! review FC3 closed. **On Android a submitted line never has a newline in the
//! platform buffer at all**, because Return left as an editor action rather
//! than as text: the stale `added` is the bare command line (`"hunter2"`, not
//! `"hunter2\n"`), so a guard keyed on the Return spends its shot without ever
//! firing.
//!
//! So every scrub arms a **one shot**, and while it is armed a snapshot has to
//! pass **two** tests — one on content, one on shape — to be let through:
//!
//! 1. **It must not be our own dropped line coming back**
//!    ([`KeystrokeSource::is_replay`], review R2-F2). The scrub records a keyed
//!    digest of the baseline it drops, plus that baseline's length in UTF-16
//!    units, and **no plaintext**; while the shot is armed, an `added` whose
//!    first *length* units digest to that value **is** the line the platform
//!    never let go of, whatever it looks like. `"hunter2"` is refused because
//!    it is the string this module just scrubbed — not because of its shape,
//!    which is that of an ordinary gesture commit and always was. Comparing a
//!    *prefix* rather than the whole add is what also catches the stale line
//!    **plus** whatever the user typed during the window (`"hunter2x"`, and
//!    iOS's `"hunter2\n"`). Such a snapshot is refused **whole**, deliberately,
//!    rather than having its genuine suffix emitted on its own: a prefix match
//!    cannot tell `"hunter2"`-then-`x` from a first commit that merely begins
//!    the same way, so emitting the tail would send a *fragment* of a keystroke
//!    run into a shell — worse than dropping it — and the re-publish has
//!    already pulled the platform back to the empty baseline the user retypes
//!    onto.
//! 2. **It must still be at most one commit's worth of input**
//!    ([`is_one_commit`]). The reasoning is the empty mirror: while the shot is
//!    armed the baseline is provably empty, so `added` is everything typed
//!    *since* the Return — which on the first snapshot after a submit is at
//!    most one keystroke or one gesture-typed word, where a shell line is many
//!    commits. This stays as the second line of defense because the digest only
//!    knows the line *this module* dropped: a platform still holding text a
//!    re-anchor had already trimmed away, or a dictation partial, is a bulk add
//!    the digest cannot recognize and the bound still rejects.
//!
//! A snapshot failing either test emits nothing, re-publishes the (empty)
//! mirror to pull the platform back into step, and — review R2-F1 — leaves the
//! shot **armed** ([`KeystrokeSource::desync`]): a refusal moves neither
//! buffer, so the empty baseline the guard is keyed on is still empty and the
//! platform is by construction still behind, and the stale window is several
//! frames long. Spending the shot there left the guard exactly one snapshot
//! deep, with the second stale frame of the same window falling through it
//! *and* through the delete guard (nothing was removed) to the host.
//!
//! Four deliberate narrowings, each pinned by a test:
//!
//! - a Return **and nothing else** is let through, because it carries no
//!   command text to replay and because a blank line at a shell prompt is
//!   ordinary typing — suppressing it would swallow every other Return the
//!   user pressed twice in a row. Being a submit of its own, it re-arms the
//!   shot;
//! - the shot is spent by the first snapshot that **actually adds something**
//!   *and is let through*, and by nothing else (review FC3, R2-F1). A snapshot
//!   that adds nothing — Gboard's `finishComposingText()` going straight to
//!   `sync()` pushes exactly such a no-op — emits nothing and therefore closes
//!   no window, so spending the shot on it would retire the guard while its
//!   precondition still held; a refused one closes no window either;
//! - a composing snapshot returns before all of this, so a composition does not
//!   spend the shot either;
//! - a dropped baseline of at most **one keystroke's worth**
//!   ([`MIN_REPLAY_DIGEST_UNITS`]) records no digest, so only the shape test
//!   applies to it. At that length the replay is one keystroke of bytes, which
//!   nothing here can tell from the same key pressed again — and refusing it
//!   would make that key permanently untypeable as the first commit after the
//!   submit, because a refusal re-arms and the digest would match every retry
//!   (`y` at a `[y/n]` prompt).
//!
//! The trades, accepted:
//!
//! - a paste, a dictated phrase, or any other multi-word commit made as the
//!   *very first* action after a Return is dropped and the user retries it —
//!   and, since a refusal re-arms, it is dropped again until one admitted
//!   keystroke lands first. That is far rarer than the desync, and a dropped
//!   paste is an annoyance where a re-executed command is an incident;
//! - re-entering the line just submitted **as a single commit** (swiping the
//!   same word again) is indistinguishable from its replay by content and is
//!   refused the same way, until any other input retires the shot. Typing it
//!   character by character is unaffected: the first character alone does not
//!   match the digest, and it spends the shot.
//!
//! **What this closes, precisely**: an exact replay of the line this module
//! dropped; that replay with anything typed after it; any stale line carrying a
//! Return (iOS's shape); any add past the one-commit bound; any add with the
//! whitespace or punctuation a shell line almost always has — and every *later*
//! frame of the same stale window, not only the first. **What it does not**,
//! recorded with the residuals above rather than claimed: a submitted line of
//! at most one keystroke's worth (`ls`, one emoji), for which no digest is
//! recorded and whose replay costs one keystroke of bytes. A frust-side bridge
//! fix making the submit path re-poll synchronously narrows the window from the
//! other end, and this guard is deliberately not conditional on it: an app
//! cannot assume the bridge version it runs against, the resync stays
//! multi-frame in the fallback path, and the emit choke point is where defense
//! in depth belongs.
//!
//! # An oversized single edit is a desync, not intent
//!
//! Dictation rewrites the whole field per partial, a replace-span rewrites a
//! word, and a bridge that applied a re-anchor one frame late reports a
//! wholesale replacement — all three arrive as one snapshot whose `removed`
//! covers most of the buffer. Emitting that many backspaces is how a
//! re-emission bug reaches the remote host, so a `removed` above
//! [`MAX_SNAPSHOT_DELETE`] emits **nothing** and re-publishes the mirror the
//! source still believes in, which is what pulls the platform back into step.
//!
//! # The latch is at the output choke point
//!
//! [`ModifierLatch`] transforms **soft-keyboard characters too** — that is the
//! incumbent's semantic, and the reason it lives on the emission path
//! ([`KeystrokeSource::emit`]) rather than in the key bar: arming `Ctrl` on the
//! bar and then typing `c` on the OS keyboard must send `0x03`. See
//! [`apply_latch`] for the transform itself.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

use frust::authoring::{EditingState, Key, Modifiers, NamedKey};
use zeroize::{Zeroize, Zeroizing};

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

/// The most units a snapshot may ever be **stored** as — one past
/// [`MIRROR_CAP`], and the reason `set_mirror` clamps (review FC2).
///
/// A pasted token or a dictation burst arrives as one snapshot far longer than
/// the cap. Its bytes are emitted from the diff either way, but keeping it
/// whole (even for the one statement until the re-anchor trims it) would grow
/// the buffer past the capacity [`KeystrokeSource::new`] pre-reserved — and a
/// `String` reallocation frees the user's plaintext where no scrub can reach
/// it.
///
/// **One past the cap, not at it**: `reanchor_if_needed` fires on
/// `len > MIRROR_CAP`, so clamping to the cap itself would silently disable
/// the re-anchor and let the source's baseline drift away from the platform's
/// buffer forever.
const MIRROR_STORE_CAP: usize = MIRROR_CAP + 1;

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

/// How many UTF-16 code units one snapshot may delete before the source reads
/// it as a **desync** rather than as typing, emits nothing and re-publishes
/// (see the [module docs](self)).
///
/// **8**, derived from what a real delete can collapse into a single snapshot
/// rather than picked round: both bridges deliver a snapshot per IME round
/// trip, a held Backspace repeats at roughly the platform's key-repeat rate
/// (~50 ms after its initial delay), and a deleted surrogate pair costs two
/// units — so two or three collapsed deletes, i.e. ≤6 units, is the observed
/// ceiling for genuine input. 8 keeps a unit of headroom over that while
/// staying far below [`MIRROR_TAIL`] (24), which is the smallest buffer a
/// whole-buffer rewrite could ever be reported against; anything at or above
/// the tail is a rewrite of everything the platform holds, never a keystroke.
///
/// The trade this makes deliberately: a *genuine* delete run longer than this
/// (a select-all-then-delete inside the IME's own mirror, which is a window
/// onto the last few typed characters and not onto the remote line at all) is
/// suppressed and the mirror restored, so the user's next backspace still
/// works. A dropped delete is an annoyance; a re-executed command is an
/// incident.
pub const MAX_SNAPSHOT_DELETE: usize = 8;

/// How many UTF-16 code units **one keystroke** can be, and therefore the
/// shortest add the post-scrub guard admits without looking at its shape at all
/// (see [`is_one_commit`]).
///
/// **2**, because a keyboard's own emoji panel commits a surrogate pair — two
/// units for one tap — and nothing a single key press produces is longer. Every
/// character a shell line is built out of (a space, a `-`, a `|`, a quote) is
/// one unit, so this is what keeps the guard from swallowing the ordinary
/// first-keystroke-after-a-Return whatever character it happens to be.
const SINGLE_KEYSTROKE_UNITS: usize = 2;

/// The longest single IME **commit** the post-scrub guard admits as genuine
/// first input (see [`is_one_commit`] and the [module docs](self)).
///
/// **16**, derived from what one commit can be rather than picked round. The
/// only multi-unit add a soft keyboard produces in one go is a gesture-typed
/// word: frust's Android bridge deliberately keeps swipe typing working on an
/// [`ImeContentType::Terminal`](frust::authoring::ImeContentType) field (it
/// sets `TYPE_TEXT_FLAG_NO_SUGGESTIONS` but **not**
/// `TYPE_TEXT_VARIATION_VISIBLE_PASSWORD`, which an S2-spike device check found
/// some IMEs read as "no gestures"), so a whole word arriving in one snapshot
/// is expected traffic here. Fifteen letters is the far tail of English word
/// length, plus the one trailing space a gesture commit appends: 16. Anything
/// longer is more than one commit, and on this seam that means a line.
///
/// The trade this makes deliberately, and the reason it is not smaller: a bound
/// tight enough to reject every short stale line would also reject the gesture
/// commits this field is set up to receive. A stale line inside the bound —
/// one bare word, `htop` or a word-shaped password — is caught by the
/// **content** half of the guard instead ([`KeystrokeSource::is_replay`]),
/// which knows the exact line the scrub dropped and therefore does not have to
/// guess from a shape.
pub const MAX_POST_SCRUB_ADD: usize = 16;

/// The shortest dropped baseline the post-scrub guard records a replay digest
/// for (see [`KeystrokeSource::scrub`] and the [module docs](self)).
///
/// **One unit past [`SINGLE_KEYSTROKE_UNITS`]**, i.e. the first length that is
/// more than one keystroke. Below it the content test buys nothing and costs
/// the user a key: a submitted line of at most one keystroke's worth strands a
/// replay worth exactly one keystroke of bytes — which no rule here can tell
/// from the same key pressed again — while refusing it would make that key
/// **permanently** untypeable as the first commit after the submit, since a
/// refusal re-arms the shot and the digest would match every retry. `y` at a
/// `[y/n]` prompt is the case that costs.
const MIN_REPLAY_DIGEST_UNITS: usize = SINGLE_KEYSTROKE_UNITS + 1;

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

/// The first `units` UTF-16 code units of `s`, rounded **inward** to a
/// character boundary so a surrogate pair is never split (asking for one unit
/// of a leading emoji yields the empty string).
///
/// Inward is the safe direction for its one caller
/// ([`KeystrokeSource::is_replay`]): the dropped baseline it is compared
/// against is whole characters, so a prefix that would have to split a pair is
/// not that baseline — and coming up **short** makes the caller's own length
/// check reject it, rather than digesting a slice that is one unit off.
fn utf16_prefix(s: &str, units: usize) -> &str {
    let mut taken = 0usize;
    let mut end = 0usize;
    for (offset, ch) in s.char_indices() {
        if taken + ch.len_utf16() > units {
            break;
        }
        taken += ch.len_utf16();
        end = offset + ch.len_utf8();
    }
    &s[..end]
}

/// Drop leading characters from `s` until it is at most `units` UTF-16 code
/// units long, character-aligned — [`utf16_suffix`] as an in-place truncation.
///
/// **In place, deliberately** (review M6): the obvious spelling
/// (`*s = utf16_suffix(s, units).to_string()`) leaves the user's typing in a
/// freed allocation nobody scrubs. `drain` shifts the tail down inside the
/// same allocation, so the only copy of the dropped head is the stale bytes
/// past the new length — which [`KeystrokeSource::scrub`] and `Zeroizing`'s
/// own drop both zero across the buffer's **whole capacity**.
fn clamp_front(s: &mut String, units: usize) {
    if len_utf16(s) <= units {
        return;
    }
    let start = s.len() - utf16_suffix(s, units).len();
    s.drain(..start);
}

/// One index at the seam, clamped into a `len`-unit buffer: a negative index
/// normalizes to the `-1` "none" sentinel `EditingState` documents, and a
/// positive one can never point past the end.
fn clamp_index(index: i32, len: i32) -> i32 {
    if index < 0 { -1 } else { index.min(len) }
}

/// `state`'s composing range, **validated** against its own text, or `None`
/// when it is not composing.
///
/// The platform's indices are taken on trust nowhere else in this module, and
/// this is the one place they are echoed straight back out
/// ([`KeystrokeSource::published`] republishes a composing snapshot verbatim
/// on every focused frame). A base past the end, an inverted range or a
/// half-set pair therefore resolves to "not composing" rather than being
/// handed back to the other bridge as a range into text that cannot hold it.
fn composing_range(state: &EditingState) -> Option<(i32, i32)> {
    if state.composing_base < 0 || state.composing_extent <= state.composing_base {
        return None;
    }
    let len = i32::try_from(len_utf16(&state.text)).unwrap_or(i32::MAX);
    let base = state.composing_base.min(len);
    let extent = state.composing_extent.min(len);
    (extent > base).then_some((base, extent))
}

/// Whether `text` carries a Return — the trigger for dropping the baseline
/// (see the [module docs](self)). Both spellings, because a pasted `"\r\n"` is
/// two Returns on the way out and must not leave either byte behind.
fn carries_return(text: &str) -> bool {
    text.contains(['\n', '\r'])
}

/// Whether `text` is a Return and **nothing else** — the one Return-carrying
/// add the stale-snapshot guard lets through (see the [module docs](self)): it
/// has no command text to replay, and a blank line at a shell prompt is
/// ordinary typing.
fn is_bare_return(text: &str) -> bool {
    matches!(text, "\n" | "\r" | "\r\n")
}

/// Whether `added` is at most **one commit's worth** of input — the
/// post-scrub replay guard's admission test (see the [module docs](self)).
///
/// While the shot is armed the mirror is provably empty, so `added` is
/// everything the user has typed *since* the Return. On the first snapshot
/// after a submit that is at most one commit; a stale line is many. The three
/// rules, in order:
///
/// 1. **A Return is admitted only bare.** A Return *with* text is the shape
///    review FC2 caught: the whole submitted line and its newline, which
///    [`text_bytes`] would map to CR and re-execute. A Return *alone* carries
///    nothing to replay (the module docs' first narrowing).
/// 2. **Any single keystroke passes**, whatever character it is
///    ([`SINGLE_KEYSTROKE_UNITS`]) — a space, a `-` or a `|` is as ordinary at
///    a shell prompt as a letter, and an emoji is one tap for two units.
/// 3. **Longer, and it has to look like one word**: at most
///    [`MAX_POST_SCRUB_ADD`] units of alphanumerics, with the single trailing
///    space a gesture commit appends allowed. `"status"` and `"status "` are
///    one commit; `"sudo -i"`, `"ls -la"` and `"cd /tmp"` are not — a shell
///    line's whitespace and punctuation is exactly what makes it more than one.
///    (Alphanumeric is Unicode-wide, so a kana→kanji conversion commit is one
///    word here too.)
///
/// A **shape** test, not a proof, and deliberately only half the guard: a stale
/// line that is itself one bare word inside the bound (`htop`, a word-shaped
/// password) is indistinguishable *by shape* from a gesture commit, and is
/// admitted here. Those are refused by the **content** test the caller runs
/// first ([`KeystrokeSource::is_replay`]), which knows the exact line the
/// scrub dropped. This rule stays as the second line of defense, because it
/// rejects the shapes that test cannot know about — a platform still holding
/// text a re-anchor had already trimmed away, a dictation partial, any bulk
/// rewrite that was never our baseline (see the [module docs](self)).
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

/// The minimal `(removed, added)` edit turning `old` into `new`: the common
/// prefix and common suffix are cancelled, `removed` counts the UTF-16 units
/// the middle of `old` held, and `added` is the middle of `new`.
///
/// Both boundaries are **character**-aligned (Rust `char`s are whole scalars),
/// so a surrogate pair can never be split across a diff boundary — the emoji
/// case that a naive UTF-16-indexed diff gets wrong.
///
/// The returned `added` is a fresh allocation holding the user's own text:
/// [`KeystrokeSource::on_snapshot`] takes ownership of it inside a
/// [`Zeroizing`] so its drop scrubs (see the [module docs](self)). A caller
/// that keeps it must do the same.
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
    /// A snapshot read as a bridge/dictation **desync** rather than as typing:
    /// one deleting more than [`MAX_SNAPSHOT_DELETE`] units, or — into the
    /// empty baseline a scrub left — one whose add begins with the line that
    /// scrub dropped ([`KeystrokeSource::is_replay`]) or is more than one
    /// commit's worth ([`is_one_commit`]), i.e. a just-submitted line replayed.
    /// Nothing is emitted, the mirror is re-published to pull the platform back
    /// into step, and the guard stays armed for the next frame of the same
    /// window (see the [module docs](self)).
    Desync,
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

/// The live composing snapshot, held the way every other buffer here is: the
/// platform's own text in a [`Zeroizing`] `String`, plus the **already
/// validated** indices ([`composing_range`], [`clamp_index`]) that arrived with
/// it.
///
/// Not an [`EditingState`] (review FC2). A composing snapshot carries the whole
/// mobile mirror — the password mid-composition included — so keeping it as a
/// plain `String` parked one unscrubbed plaintext copy per composing keystroke,
/// dropped without zeroing when the next one replaced it: exactly the pattern
/// review M6 converted the mirror and the history away from.
/// [`KeystrokeSource::published`] rebuilds an `EditingState` from this on
/// demand instead.
struct ComposingEcho {
    /// The platform's text, verbatim. Never clamped the way the mirror is: a
    /// composition's indices are indices into *this*, so trimming it would
    /// hand the bridge a range into text that cannot hold it.
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

    /// The state to publish — rebuilt per call rather than stored, so the
    /// plain copy lives only as long as the platform's own round trip (the
    /// [`published`](KeystrokeSource::published) residual, and no more).
    fn state(&self) -> EditingState {
        EditingState {
            text: self.text.as_str().to_owned(),
            selection_base: self.selection_base,
            selection_extent: self.selection_extent,
            composing_base: self.composing_base,
            composing_extent: self.composing_extent,
        }
    }

    /// How many UTF-16 units of the user's own text this is holding — what
    /// [`buffered_units`](KeystrokeSource::buffered_units) counts it as.
    fn units(&self) -> usize {
        len_utf16(&self.text)
    }
}

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
    ///
    /// [`Zeroizing`] because this is the user's own typing, secrets included
    /// (see the [module docs](self)): every overwrite and the final drop scrub
    /// the buffer rather than leaving plaintext behind.
    mirror: Zeroizing<String>,
    /// The user's own recent typing, [`HISTORY_CAP`] units of it. A re-anchor
    /// refills the mirror from here; nothing else reads it. [`Zeroizing`] for
    /// the same reason [`mirror`](Self::mirror) is.
    history: Zeroizing<String>,
    /// The live composing snapshot, while one exists — echoed verbatim by
    /// [`published`](Self::published) so the paint-pass republish cannot
    /// destroy the platform's composition.
    composing: Option<ComposingEcho>,
    /// **One shot, armed by every [`scrub`](Self::scrub)**: the baseline was
    /// just dropped to empty, so the platform may still be holding the line
    /// that was submitted (see the [module docs](self)). Spent by the next
    /// snapshot that actually **adds** something *and is let through* — the
    /// window it guards is closed by that snapshot, and by nothing else:
    ///
    /// - a snapshot that adds nothing leaves it armed, because the empty
    ///   baseline outlives such a snapshot (review FC3);
    /// - a snapshot the guard **refuses** re-arms it inside
    ///   [`desync`](Self::desync), because a refusal emits nothing and moves
    ///   neither buffer: what the platform holds is still not what this source
    ///   believes, and the stale window can be several frames long (review
    ///   R2-F1). That widens the flag's meaning from "the baseline was just
    ///   dropped" to "…or the last snapshot was refused", which is deliberate
    ///   on the oversized-`removed` branch too: a platform that has just proved
    ///   itself out of step gets one commit's worth at a time until one
    ///   snapshot reconciles.
    baseline_dropped: bool,
    /// A **keyed digest and UTF-16 length** of the baseline the last
    /// [`scrub`](Self::scrub) dropped — the content half of the post-scrub
    /// guard, and the only thing about a dropped line that survives it
    /// (review R2-F2).
    ///
    /// **No plaintext**: a `u64` from [`replay_seed`](Self::replay_seed) plus a
    /// count, which is what lets the guard recognize its own just-scrubbed line
    /// coming back from a platform that has not caught up — `"hunter2"` is
    /// refused because it *is* that line, not because of how it looks. `None`
    /// when the dropped baseline was at most one keystroke's worth
    /// ([`MIN_REPLAY_DIGEST_UNITS`]) and when the shot has been spent.
    dropped: Option<(u64, usize)>,
    /// This instance's own hash seed, so [`dropped`](Self::dropped)'s digest is
    /// meaningless anywhere else: not comparable between two sources, not
    /// comparable across two processes, and not a value that says anything
    /// about the line without the in-process key.
    replay_seed: RandomState,
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
    ///
    /// Both buffers are allocated at their steady-state capacity up front:
    /// a `String` that grows reallocates, and a reallocation frees the old
    /// plaintext where no scrub can reach it (`zeroize`'s own documented
    /// caveat). Three bytes per UTF-16 unit is an upper bound for any text at
    /// all: it covers every character below the surrogate range, and a
    /// surrogate pair is 4 bytes for 2 units. The mirror's bound is
    /// [`MIRROR_STORE_CAP`] rather than [`MIRROR_CAP`] because that is what
    /// `set_mirror` clamps to (one unit past the cap, so the re-anchor still
    /// fires).
    ///
    /// The history reserves **one unit more** than [`HISTORY_CAP`]
    /// (review FC3). `clamp_front` rounds *outward* to a character boundary
    /// ([`utf16_suffix`]), so a clamp landing mid-surrogate-pair legitimately
    /// leaves `HISTORY_CAP + 1` units — one byte past a `HISTORY_CAP * 3`
    /// reserve, and therefore one reallocation. That matters here and nowhere
    /// else: `set_mirror` **appends** to the live history, so a growth would
    /// free the user's own plaintext, where the mirror and the composing echo
    /// are rewritten `zeroize()`-then-`push_str` and can only ever free zeros.
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

    /// The editing state to publish right now.
    ///
    /// The mirror with the caret collapsed at its end — **except** while a
    /// composition is live, where it is the platform's own snapshot echoed
    /// back (rebuilt from the scrubbed [`ComposingEcho`]), so re-publishing it
    /// is a no-op on both bridges instead of a composition-destroying
    /// wholesale replace.
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

    /// The mirror's current text — a **test-only** accessor, and the one thing
    /// here that hands out the user's own plaintext, so it is compiled out of
    /// every non-test build. Production reads the mirror through
    /// [`published`](Self::published) (which the platform needs anyway) and
    /// asserts on [`buffered_units`](Self::buffered_units) instead.
    #[cfg(test)]
    pub fn mirror(&self) -> &str {
        &self.mirror
    }

    /// How many UTF-16 code units of the user's own typing the source is
    /// holding — the **content-free** counterpart of
    /// [`mirror`](Self::mirror), and what a scrub boundary is asserted on
    /// (`0` is the only value that means "nothing retained").
    ///
    /// An upper bound rather than an exact count: the mirror is a suffix of
    /// the history by invariant, so a non-zero answer counts those units
    /// twice, and a live composition is counted on top of both. That is the
    /// safe direction for the one question this accessor exists to answer —
    /// and the composing echo belongs in the answer, because it holds the
    /// user's own text exactly as the other two do (review FC2).
    pub fn buffered_units(&self) -> usize {
        len_utf16(&self.mirror)
            + len_utf16(&self.history)
            + self.composing.as_ref().map_or(0, ComposingEcho::units)
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

    // -- the scrub -----------------------------------------------------------

    /// **Drop everything the user typed**: both buffers, the composing echo,
    /// and the backspace arm that only means anything against them.
    ///
    /// Zeroing rather than clearing — `zeroize` writes across each buffer's
    /// whole capacity, so the stale tail an in-place truncation leaves behind
    /// goes too. Capacity survives, so a scrub never reallocates.
    ///
    /// After this the diff baseline is **empty**: the next snapshot is diffed
    /// against nothing, which is exactly right at every boundary this is
    /// called from — a submitted line, a blur, `read_only`, teardown (see the
    /// [module docs](self), and [`InputSeam`](super::InputSeam) for the
    /// wiring). The **latch** is deliberately left alone: an armed `Ctrl` is
    /// not a secret, and clearing it would owe the key bar a signal write from
    /// paths (a paint-pass blur) that must not make one.
    ///
    /// Dropping the baseline is also what arms
    /// [`baseline_dropped`](Self::baseline_dropped) — the one shot that covers
    /// the frame the *platform* may still be holding the old line for (review
    /// FC2; see the [module docs](self)) — and what records
    /// [`dropped`](Self::dropped), the shot's **content** test: a keyed digest
    /// of the baseline about to be zeroed, plus its length in UTF-16 units, so
    /// that same line arriving back from a platform that has not caught up is
    /// recognizable as a replay whatever its shape (review R2-F2). A digest is
    /// **not** plaintext and is not retained past the shot it belongs to; a
    /// baseline of at most one keystroke's worth records none at all
    /// ([`MIN_REPLAY_DIGEST_UNITS`]).
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

    /// A line has been submitted: drop the baseline and report that the caller
    /// must re-publish (see the [module docs](self)' Return rule).
    ///
    /// Counted as a re-anchor because that is exactly what it costs — one
    /// platform `restartInput`, on the rarest keystroke there is.
    fn submit_line(&mut self) -> bool {
        self.scrub();
        self.reconciles += 1;
        true
    }

    /// The answer to a snapshot the source reads as a **desync** rather than as
    /// typing: emit nothing, move nothing, and re-publish the state the source
    /// still believes in — which is what pulls the platform back into step.
    ///
    /// Both guards end here (see the [module docs](self)): the oversized
    /// `removed`, and the stale post-scrub replay. The backspace arm goes too —
    /// the echo it was waiting for is not in a snapshot like this one.
    ///
    /// **Refusing a snapshot re-arms the one shot** (review R2-F1). This is the
    /// one place that both refuses a snapshot *and* provably leaves the
    /// baseline it was diffed against exactly as it was — nothing is emitted,
    /// nothing is stored, only a re-publish goes out — so the guard's own
    /// precondition still holds when this returns, and the flag has to say so.
    /// Coupling the re-arm to the invariant here rather than enumerating call
    /// sites is the same argument that puts the *arm* inside
    /// [`scrub`](Self::scrub). Without it the guard was exactly **one snapshot**
    /// deep: the platform's stale window is several frames long (Android's
    /// `performEditorAction` only counts down `imeResyncFrames`), so the second
    /// stale snapshot of the same window would find the shot spent, nothing
    /// removed for the delete guard to see, and the whole previous command line
    /// would go to the host.
    ///
    /// It cannot livelock: a desync emits nothing, moves neither buffer and
    /// re-publishes the state the source already believed in, so repeating it
    /// is idempotent — a genuinely oversized commit is simply dropped, frame
    /// after frame, until one snapshot the guard admits (one ordinary
    /// keystroke) retires the arm.
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

    /// A keyed digest of `text` under this instance's own seed — the only form
    /// a dropped baseline is ever kept in (see [`dropped`](Self::dropped)).
    fn digest(&self, text: &str) -> u64 {
        self.replay_seed.hash_one(text)
    }

    /// Whether `added` **begins with the baseline the last scrub dropped** —
    /// the content half of the post-scrub guard (see the [module docs](self)).
    ///
    /// A *prefix* rather than the whole add, so the stale line plus whatever
    /// the user typed during the window (`"hunter2x"`, and iOS's
    /// `"hunter2\n"`) is caught as well as the exact replay. The length check
    /// is what makes the comparison sound: [`utf16_prefix`] rounds inward, so
    /// an add too short to hold the baseline — or one whose boundary would
    /// split a surrogate pair — comes up short and is not a match.
    fn is_replay(&self, added: &str) -> bool {
        let Some((digest, units)) = self.dropped else {
            return false;
        };
        let head = utf16_prefix(added, units);
        len_utf16(head) == units && self.digest(head) == digest
    }

    /// Record one composing snapshot, rewriting the echo's buffer **in place**
    /// so a composing keystroke leaves no unscrubbed plaintext copy behind
    /// (review FC2 — the same rule the mirror follows).
    ///
    /// `selection`/`composing` are `(base, extent)` pairs the caller has
    /// already validated against `text`.
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
    ///
    /// Takes a `&str` and rewrites both buffers **in place** rather than
    /// taking a `String` it could park: a per-keystroke `state.text.clone()`
    /// handed over here would be one unscrubbed plaintext allocation per
    /// keystroke (review M6).
    ///
    /// Only the **clamped suffix** is stored (review FC2). A pasted token or a
    /// dictation burst arrives as one oversized snapshot; its bytes were
    /// already emitted from the diff, and what is kept here is only the next
    /// diff's baseline — so keeping it whole bought nothing and cost two
    /// things: the buffers would grow past the capacity [`new`](Self::new)
    /// pre-reserved (a reallocation frees the old plaintext where no scrub can
    /// reach it), and the whole secret would be resident, unclamped, until the
    /// re-anchor trimmed it. [`MIRROR_STORE_CAP`] is one unit past the cap
    /// precisely so [`reanchor_if_needed`](Self::reanchor_if_needed) still sees
    /// the overflow and re-anchors.
    ///
    /// The history is clamped **before** the push rather than after it, which
    /// is the same result — a drop from the front can only ever come out of
    /// the head, since one snapshot is now at most [`MIRROR_STORE_CAP`] units
    /// — without the oversized intermediate. This also retires the transient
    /// state where the mirror was longer than the history it is supposed to be
    /// a suffix of: `head` below can no longer underflow.
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
        // The tail as a byte range into history, so the refill below copies
        // straight between the two scrubbed buffers instead of through a
        // plaintext temporary (review M6).
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
    /// against the mirror — **the diff engine**.
    ///
    /// Order matters, and this is the order:
    ///
    /// 1. **A live composing range wins**: report, emit nothing, move nothing.
    ///    Its indices are validated first ([`composing_range`]) — this is the
    ///    one thing the source echoes back to the platform verbatim. The
    ///    post-scrub one shot is deliberately **not** spent here: a composition
    ///    emits nothing, so the stale window it guards is still open behind it.
    /// 2. Diff against the mirror, then the desync guards, which all emit
    ///    nothing, move nothing, re-publish and leave the one shot armed (see
    ///    the [module docs](self)): into the shot the last scrub armed, an
    ///    `added` that begins with the baseline that scrub dropped
    ///    ([`is_replay`](Self::is_replay)) or that is bigger than one commit's
    ///    worth ([`is_one_commit`]) — a submitted line being replayed, with or
    ///    without the Return the platform's own submit path decides — and, on
    ///    any snapshot, an oversized `removed`.
    /// 3. Otherwise a shortening consumes any armed `Key::Named(Backspace)`
    ///    echo first (the dedupe rule), and any snapshot that does not shorten
    ///    retires the arm.
    /// 4. Emit the backspaces, then the added text — each through the latch.
    /// 5. A Return in what was added **drops the baseline** (the module docs'
    ///    Return rule); otherwise accept the snapshot as the new mirror and
    ///    re-anchor if the window has drifted out of `FLOOR..=CAP`.
    pub fn on_snapshot(&mut self, state: &EditingState) -> Emission {
        if let Some((base, extent)) = composing_range(state) {
            // Report-only: re-publishing mid-composition would destroy the
            // platform's own composition state (see the module docs).
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
        // The one plaintext temporary on this path: owned here so its drop
        // scrubs it (review M6).
        let added = Zeroizing::new(added);

        if !added.is_empty() {
            // The last scrub dropped OUR baseline; the platform may still hold
            // the line that was submitted. Diffed against nothing, that stale
            // snapshot reads as one `added` run carrying the whole line — with
            // its Return on iOS, and WITHOUT one on Android, where Return left
            // as an editor action rather than as text (review FC3). Emitting
            // either re-runs the command on the remote host; the
            // oversized-delete guard below cannot catch them, because nothing
            // was removed (review FC2). So while the shot is armed, only one
            // commit's worth of input is let through.
            //
            // Two tests, and it has to pass both: it must not BE the line the
            // scrub dropped (`is_replay` — content, whatever its shape, which
            // is what catches the word-shaped `"hunter2"` the shape rule
            // admits), and it must still be at most one commit's worth
            // (`is_one_commit` — shape, which is what catches the lines the
            // digest cannot know about).
            //
            // The shot is spent here and only here, by a snapshot that both
            // adds something and is let through: a snapshot that adds nothing
            // (Gboard's `finishComposingText()` → `sync()`) emits nothing and
            // closes no window, so it must not retire a guard whose
            // precondition — the empty baseline — outlives it, and a REFUSED
            // one re-arms inside `desync` for exactly the same reason.
            let armed = std::mem::take(&mut self.baseline_dropped);
            if armed {
                if self.is_replay(&added) || !is_one_commit(&added) {
                    return self.desync();
                }
                // Let through: the window is closed, so the fingerprint of the
                // submitted line goes with the shot it belonged to.
                self.dropped = None;
            }
        }

        if removed > MAX_SNAPSHOT_DELETE {
            // A whole-buffer replacement, not typing: emit NOTHING and re-publish
            // the mirror the source still believes in.
            return self.desync();
        }

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
            (_, text) if is_bare_return(text) => InputClass::Enter,
            (_, "") if removed > 0 => InputClass::Backspace,
            (_, "") => InputClass::None,
            _ => InputClass::Text,
        };

        // A submitted line is dropped rather than kept as a baseline, so no
        // `\n` can live in the mirror to be re-emitted later — including
        // inside a multi-line paste, which is several Returns and one of
        // these (see the module docs).
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

    /// A desktop (winit) commit — the composed text the platform finished.
    ///
    /// Emitted straight through: there is no platform mirror to diff against on
    /// desktop, so the mirror is left alone — unless the commit carried a
    /// Return, which drops the baseline on every path alike (the module docs'
    /// Return rule), keeping that rule one rule rather than a mobile special
    /// case.
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

    /// A focus-routed key event: Android's `IME_ACTION_DONE`-as-`Enter`, and
    /// every real key on the desktop preview.
    ///
    /// A `Key::Named(Backspace)` **arms the dedupe** (see the module docs): it
    /// emits its byte here and the reconciler swallows the platform's echo of
    /// the same delete.
    ///
    /// Android's Return arrives here rather than in a snapshot, so this is the
    /// other half of the Return rule: `IME_ACTION_DONE` drops the baseline
    /// exactly as iOS's literal `"\n"` does, and the secret typed before it
    /// goes with it.
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
    fn utf16_prefix_rounds_inward_and_never_splits_a_pair() {
        assert_eq!(utf16_prefix("abcdef", 3), "abc");
        assert_eq!(utf16_prefix("abc", 9), "abc");
        assert_eq!(utf16_prefix("abc", 0), "");
        // One unit of a LEADING surrogate pair yields nothing rather than half
        // a character — the inward rounding the replay digest depends on.
        assert_eq!(utf16_prefix("\u{1f600}ab", 1), "");
        assert_eq!(utf16_prefix("\u{1f600}ab", 2), "\u{1f600}");
        assert_eq!(utf16_prefix("\u{1f600}ab", 3), "\u{1f600}a");
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

        // CONTRACT CHANGE (review M7, guard 1). This test used to leave
        // `"ls\n"` sitting in the mirror as the next diff's baseline. It no
        // longer does: a submitted line is dropped, so the literal `\n` iOS
        // accumulates can never be part of a later bulk re-emission — which,
        // because `text_bytes` maps `\n` to CR, would have re-executed `ls`
        // on the remote host.
        assert!(emission.reconciled, "the window re-anchors, to empty");
        assert_eq!(source.mirror(), "", "no `\\n` is left in the mirror");
        assert_eq!(source.buffered_units(), 0, "…and none in history either");
    }

    #[test]
    fn the_keystroke_after_a_return_diffs_against_an_empty_baseline() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        source.on_snapshot(&snapshot("ls\n"));

        // The platform reconciled to the republished (empty) mirror inside the
        // same round trip, so its next snapshot is built on nothing — which is
        // exactly what `type_text` models.
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

    // -----------------------------------------------------------------
    // The stale-snapshot guard (review FC2): the frame the PLATFORM may
    // still be holding the submitted line for
    // -----------------------------------------------------------------

    #[test]
    fn a_stale_snapshot_after_a_return_replays_nothing_to_the_host() {
        let mut source = KeystrokeSource::new();
        // Positive precondition: the line really was typed, and really was sent.
        assert_eq!(hex(&type_text(&mut source, "ls")), "6c73");
        let submitted = source.on_snapshot(&snapshot("ls\n"));
        assert_eq!(hex(&submitted.bytes), "0d", "the Return went out");
        assert_eq!(source.mirror(), "", "…and guard 1 dropped the baseline");
        let sent = source.emitted();

        // THE window. This snapshot is built from the PLATFORM's own stale
        // buffer — deliberately not from `source.mirror()`, which is what
        // `type_text` would do and what would hide the bug entirely. Diffed
        // against the empty baseline it reads `added = "ls\n"`, whose `\n`
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
        // THE Android case (review FC3), and the reason the guard cannot be
        // keyed on the Return: `IME_ACTION_DONE` leaves as an editor action,
        // never as text, and `performEditorAction` does not re-poll — it counts
        // down `imeResyncFrames`, its own comment recording that the field is
        // cleared on the next-frame REBUILD. So the `Editable` keeps the
        // pre-submit line, WITHOUT a newline, for those frames.
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

        // RAW: built from the PLATFORM's own stale buffer, deliberately not
        // from `source.mirror()` — a snapshot built from our own mirror is
        // what hid this shape in the first place.
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
        // The other platform's shape of the same window, kept as its own case:
        // iOS accumulates the literal Return in its mirror, so the stale buffer
        // arrives WITH it — which is what review FC2 closed first, and what the
        // Android shape above proved was only half the finding.
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
        // The guard admits one commit's worth, so every shape a real first
        // keystroke has survives it: one character, one emoji tap (two units
        // for one commit), and one gesture-typed word — which the Android
        // bridge deliberately keeps working on a terminal field — with or
        // without the trailing space a gesture commit appends.
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

            // RAW again: the platform reconciled to that empty state inside the
            // same round trip, so this snapshot is the first commit on it.
            let emission = source.on_snapshot(&snapshot(first));
            assert_eq!(hex(&emission.bytes), expected, "{first:?} must land");
            assert_eq!(emission.class, InputClass::Text);
            assert_eq!(source.mirror(), first, "…and become the new baseline");
        }
    }

    #[test]
    fn a_no_op_snapshot_does_not_spend_the_stale_shot() {
        // Gboard's `finishComposingText()` goes straight to `sync()`, which
        // pushes the mirror unchanged: a snapshot that adds nothing, emits
        // nothing and closes no window. Spending the one shot on it would
        // retire the guard while its precondition — the empty baseline — still
        // held, and the stale replay behind it would fall through both guards
        // (review FC3).
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
            ("ls\n", false, "a Return WITH text is the FC2 shape"),
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
        // A blank line at a shell prompt: Return pressed twice. The second one
        // carries no command text to replay, so the guard's narrowing lets it
        // through rather than swallowing every other Enter (see the module
        // docs' Return rule).
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        assert_eq!(hex(&source.on_snapshot(&snapshot("ls\n")).bytes), "0d");

        let blank = source.on_snapshot(&snapshot("\n"));
        assert_eq!(hex(&blank.bytes), "0d", "the blank line still submits");
        assert_eq!(blank.class, InputClass::Enter);
        assert_eq!(source.buffered_units(), 0, "and still keeps nothing");
    }

    // -----------------------------------------------------------------
    // The guard is more than one snapshot deep (review R2-F1), and it
    // compares CONTENT, not just shape (review R2-F2)
    // -----------------------------------------------------------------

    #[test]
    fn a_second_stale_snapshot_in_the_same_window_replays_nothing_either() {
        // THE R2-F1 case. `performEditorAction` does not re-poll: it counts
        // down `imeResyncFrames`, so the SAME stale `Editable` can be polled on
        // several frames. A guard that spent its shot on the first of them left
        // the second with `armed == false` and `removed == 0` — no guard, and
        // no delete for the other guard to see — so the whole previous command
        // line went to the host.
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
        // The other branch of the same defect (review R2-F1): a refusal arms
        // the shot wherever it fires, so a platform that has just proved itself
        // out of step gets one commit's worth at a time until one snapshot the
        // source can reconcile arrives.
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
        // THE R2-F2 case, and the example the module docs are written around.
        // On Android the stale buffer is the pre-submit line WITHOUT a newline,
        // so `"hunter2"` — seven alphanumeric units — is exactly the shape
        // `is_one_commit` exists to admit. What refuses it is that it IS the
        // line the scrub just dropped.
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
        // The platform is behind AND the user kept typing, so the stale buffer
        // is the dropped line with a genuine keystroke on the end. The prefix
        // digest sees it; the DECISION (module docs, guard test 1) is to refuse
        // the whole snapshot rather than emit the `x` on its own — a prefix
        // match cannot tell this from a first commit that merely begins the
        // same way, and a fragment of a keystroke run in a shell is worse than
        // a dropped one.
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

        // The cost of that decision, and its recovery: the user's `x` was
        // dropped with the replay, and retyping it onto the re-published
        // baseline lands.
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
        // Two sources, the same submitted line: the recorded lengths match
        // (that part IS the line's length), and the digests do not, because the
        // seed is this instance's own `RandomState` rather than a fixed
        // constant that would be comparable across sources — and processes.
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
        // The bound on the content test (`MIN_REPLAY_DIGEST_UNITS`) and the
        // residual it leaves, both in one place.
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

        // And the reason the bound is where it is: the identical NEXT
        // keystroke. A digest at this length would refuse it — every time,
        // since a refusal re-arms — so `y` at a `[y/n]` prompt would be
        // untypeable.
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

    // -----------------------------------------------------------------
    // The desync guard, and the scrub
    // -----------------------------------------------------------------

    #[test]
    fn a_whole_buffer_replacement_emits_nothing_and_re_publishes() {
        // Dictation rewriting the field per partial, a replace-span rewrite, or
        // a bridge one frame behind: all three arrive as one snapshot that
        // deletes the buffer. Emitting that is how previously typed commands
        // get replayed to the remote host.
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
        // A composition ALONE, so what is asserted is the echo's own
        // retention rather than the mirror's (review FC2: the echo holds the
        // whole platform mirror, password mid-composition included).
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
        // The echo's buffer is rewritten in place (zeroize + push) rather than
        // reallocated per composing keystroke, so a shrinking composition must
        // not leave the longer one's tail behind.
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
        // The one thing this module hands the platform verbatim, so the
        // indices are checked against the text that arrived with them rather
        // than taken on trust.
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

        // An inverted/unusable range is not a composition at all: the snapshot
        // takes the ordinary diff path instead of being echoed back as one.
        // Nothing is invented either way — the text is still the platform's.
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
    fn an_oversized_snapshot_is_never_stored_whole() {
        // `set_mirror` is called directly because the re-anchor immediately
        // after it would otherwise hide exactly what is under test: the
        // TRANSIENT. A snapshot bigger than the pre-reserved capacity, stored
        // whole, reallocates — and a reallocation frees the user's plaintext
        // where no scrub can reach it (review FC2).
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
