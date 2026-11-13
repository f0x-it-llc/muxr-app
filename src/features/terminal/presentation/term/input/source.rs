//! [`KeystrokeSource`]: diffs IME snapshots against a rolling mirror window and guards desync.
//! The window re-anchors on demand, never to empty, and is sound only because both bridges apply
//! the republished state synchronously inside the round trip that delivered the keystroke.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

use frust::authoring::{EditingState, Key, Modifiers, NamedKey};
use zeroize::{Zeroize, Zeroizing};

use super::bytes::{BACKSPACE_BYTE, BarKey, modified_sequence, named_key_bytes, text_bytes};
use super::latch::{ModifierLatch, apply_latch, single_unit};
use super::utf16::{clamp_front, clamp_index, diff, len_utf16, utf16_prefix, utf16_suffix};

/// UTF-16 units the published mirror may reach before a re-anchor.
pub const MIRROR_CAP: usize = 96;

/// One past the cap, since `reanchor_if_needed` fires on `len > MIRROR_CAP`; more would reallocate.
const MIRROR_STORE_CAP: usize = MIRROR_CAP + 1;

/// The gap to [`MIRROR_CAP`] is how many characters may be typed between platform `restartInput`s.
pub const MIRROR_TAIL: usize = 24;

/// Never zero: a delete is only observable as a shortening.
pub const MIRROR_FLOOR: usize = 4;

/// Past it, the oldest typing cannot be backspaced into.
pub const HISTORY_CAP: usize = 256;

/// A ceiling on un-echoed `Key::Named(Backspace)` arms, not a queue.
pub const MAX_PENDING_BACKSPACES: usize = 4;

/// UTF-16 units one snapshot may delete before it reads as a desync. Two or three collapsed deletes
/// of a surrogate pair (6) is the genuine ceiling; 8 keeps headroom below [`MIRROR_TAIL`].
pub const MAX_SNAPSHOT_DELETE: usize = 8;

/// 2: an emoji panel commits a surrogate pair for one tap.
const SINGLE_KEYSTROKE_UNITS: usize = 2;

/// 16: a gesture-typed word of fifteen letters plus its trailing space. A word-shaped stale line
/// is caught by the guard's content test instead.
pub const MAX_POST_SCRUB_ADD: usize = 16;

/// Below it a replay is one keystroke no rule can tell from the same key pressed again, and
/// refusing it would make that key permanently untypeable.
const MIN_REPLAY_DIGEST_UNITS: usize = SINGLE_KEYSTROKE_UNITS + 1;

/// Validated against its own text because these indices are echoed back; a bad range is `None`.
fn composing_range(state: &EditingState) -> Option<(i32, i32)> {
    if state.composing_base < 0 || state.composing_extent <= state.composing_base {
        return None;
    }
    let len = i32::try_from(len_utf16(&state.text)).unwrap_or(i32::MAX);
    let base = state.composing_base.min(len);
    let extent = state.composing_extent.min(len);
    (extent > base).then_some((base, extent))
}

/// Both spellings: a pasted `"\r\n"` is two Returns on the way out.
fn carries_return(text: &str) -> bool {
    text.contains(['\n', '\r'])
}

/// The one Return-carrying add the stale-snapshot guard admits: it has no command text to replay.
fn is_bare_return(text: &str) -> bool {
    matches!(text, "\n" | "\r" | "\r\n")
}

/// Post-scrub shape test: a bare Return, any single keystroke, or one word of at most
/// [`MAX_POST_SCRUB_ADD`] alphanumerics plus a trailing space (`"status "`, never `"sudo -i"`).
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputClass {
    /// Nothing derivable: our own echo, a composition bracket, a latch tap.
    #[default]
    None,
    Text,
    Backspace,
    Enter,
    Composing,
    Sequence,
    Latch,
    /// An oversized delete or a replayed submitted line: nothing emitted, mirror re-published.
    Desync,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Emission {
    pub bytes: Vec<u8>,
    /// The caller must publish [`KeystrokeSource::published`] so the platform mirror follows.
    pub reconciled: bool,
    pub class: InputClass,
}

impl Emission {
    fn inert(class: InputClass) -> Self {
        Self {
            bytes: Vec::new(),
            reconciled: false,
            class,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty() && !self.reconciled && self.class == InputClass::None
    }
}

/// Not an `EditingState`: a composing snapshot carries the whole mirror, so it is held `Zeroizing`.
struct ComposingEcho {
    /// Never clamped: a composition's indices are indices into this.
    text: Zeroizing<String>,
    selection_base: i32,
    selection_extent: i32,
    composing_base: i32,
    composing_extent: i32,
}

impl ComposingEcho {
    /// Pre-allocated at the mirror's bound so an ordinary composition never grows it.
    fn new() -> Self {
        Self {
            text: Zeroizing::new(String::with_capacity(MIRROR_STORE_CAP * 3)),
            selection_base: -1,
            selection_extent: -1,
            composing_base: -1,
            composing_extent: -1,
        }
    }

    /// Rebuilt per call so the plain copy lives only as long as the platform round trip.
    fn state(&self) -> EditingState {
        EditingState {
            text: self.text.as_str().to_owned(),
            selection_base: self.selection_base,
            selection_extent: self.selection_extent,
            composing_base: self.composing_base,
            composing_extent: self.composing_extent,
        }
    }

    fn units(&self) -> usize {
        len_utf16(&self.text)
    }
}

/// Shared behind `Rc<RefCell<_>>` by the grid and key bar so both emit through one latch. Buffers
/// are `Zeroizing` and rewritten in place; the unbounded composing echo is zeroize-then-push.
pub struct KeystrokeSource {
    /// The diff baseline; always a suffix of `history`.
    mirror: Zeroizing<String>,
    history: Zeroizing<String>,
    /// Echoed verbatim by `published` so the paint-pass republish cannot destroy the composition.
    composing: Option<ComposingEcho>,
    /// Armed by every scrub, spent by the next adding snapshot let through, re-armed by `desync`.
    baseline_dropped: bool,
    /// Keyed digest and UTF-16 length of the dropped baseline, never plaintext.
    dropped: Option<(u64, usize)>,
    /// Per-instance seed, so a digest is meaningless outside this source.
    replay_seed: RandomState,
    /// Armed by `Key::Named(Backspace)`, consumed by the matching snapshot shortening.
    pending_backspaces: usize,
    latch: ModifierLatch,
    revision: u64,
    reconciles: u64,
    emitted: usize,
}

impl Default for KeystrokeSource {
    fn default() -> Self {
        Self::new()
    }
}

impl KeystrokeSource {
    /// Reserved at steady-state capacity (3 bytes per UTF-16 unit), since growth frees plaintext.
    /// History gets one unit more: an outward clamp can leave `HISTORY_CAP + 1` units.
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

    /// While composing, the platform's own snapshot, so re-publishing is a no-op on both bridges.
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

    pub fn latch(&self) -> ModifierLatch {
        self.latch
    }

    /// Test-only: the one accessor that hands out plaintext.
    #[cfg(test)]
    pub fn mirror(&self) -> &str {
        &self.mirror
    }

    /// An upper bound (the mirror is also inside history); `0` means nothing is retained.
    pub fn buffered_units(&self) -> usize {
        len_utf16(&self.mirror)
            + len_utf16(&self.history)
            + self.composing.as_ref().map_or(0, ComposingEcho::units)
    }

    pub fn reconciles(&self) -> u64 {
        self.reconciles
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn emitted(&self) -> usize {
        self.emitted
    }

    pub fn is_composing(&self) -> bool {
        self.composing.is_some()
    }

    /// Zeroizes each buffer's whole capacity without reallocating, and arms the post-scrub guard.
    /// The latch survives: not a secret, and a paint-pass blur must not write the key bar's signal.
    pub fn scrub(&mut self) {
        let units = len_utf16(&self.mirror);
        self.dropped = if units >= MIN_REPLAY_DIGEST_UNITS {
            Some((self.digest(&self.mirror), units))
        } else {
            None
        };
        self.mirror.zeroize();
        self.history.zeroize();
        self.composing = None;
        self.pending_backspaces = 0;
        self.baseline_dropped = true;
        self.revision += 1;
    }

    /// Counted as a re-anchor: it costs one platform `restartInput`.
    fn submit_line(&mut self) -> bool {
        self.scrub();
        self.reconciles += 1;
        true
    }

    /// Emits nothing and re-publishes the believed state. Re-arms the shot, since the baseline is
    /// unchanged; idempotent, so it cannot livelock.
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

    fn digest(&self, text: &str) -> u64 {
        self.replay_seed.hash_one(text)
    }

    /// A prefix test, so the stale line plus typing during the window (`"hunter2x"`) is caught too;
    /// sound because `utf16_prefix` rounds inward.
    fn is_replay(&self, added: &str) -> bool {
        let Some((digest, units)) = self.dropped else {
            return false;
        };
        let head = utf16_prefix(added, units);
        len_utf16(head) == units && self.digest(head) == digest
    }

    /// Rewrites the echo in place so a composing keystroke leaves no plaintext copy.
    fn set_composing(&mut self, text: &str, selection: (i32, i32), composing: (i32, i32)) {
        let echo = self.composing.get_or_insert_with(ComposingEcho::new);
        echo.text.zeroize();
        echo.text.push_str(text);
        echo.selection_base = selection.0;
        echo.selection_extent = selection.1;
        echo.composing_base = composing.0;
        echo.composing_extent = composing.1;
    }

    /// The one place bytes are produced; a single-unit emission disarms both flags together.
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

    /// Picks the variant an armed modifier has and consumes only that flag; Ctrl/Alt on a non-F-key
    /// stay armed and never reach the bytes.
    fn emit_sequence(&mut self, sequence: &'static [u8], transient: ModifierLatch) -> Vec<u8> {
        let chosen = modified_sequence(sequence, self.latch.union(transient));
        let out = self.emit(chosen.map_or(sequence, |(variant, _)| variant), transient);
        if let Some((_, spent)) = chosen {
            self.latch.shift &= !spent.shift;
            self.latch.ctrl &= !spent.ctrl;
            self.latch.alt &= !spent.alt;
        }
        out
    }

    /// Rewrites both buffers in place and stores only the clamped suffix, so no plaintext
    /// escapes a scrub. History is clamped before the push so `head` cannot underflow.
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

    /// The shrink case needs history to refill from, else every snapshot would republish.
    fn reanchor_if_needed(&mut self) -> bool {
        let len = len_utf16(&self.mirror);
        let over = len > MIRROR_CAP;
        let under = len < MIRROR_FLOOR;
        if !over && !under {
            return false;
        }
        // A byte range, so the refill copies between scrubbed buffers with no temporary.
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

    /// Order: a live composition only reports and leaves the post-scrub shot armed; then the desync
    /// guards; a shortening consumes an armed backspace echo; backspaces and added text go through
    /// the latch; a Return in the add drops the baseline, else the snapshot becomes the mirror.
    pub fn on_snapshot(&mut self, state: &EditingState) -> Emission {
        if let Some((base, extent)) = composing_range(state) {
            // Report-only: re-publishing mid-composition destroys the platform's composition.
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
        // Owned here so its drop scrubs it.
        let added = Zeroizing::new(added);

        if !added.is_empty() {
            // The platform may still hold the dropped line (Return on iOS, none on Android) and
            // nothing was removed. The shot is spent only here; a refusal re-arms in `desync`.
            let armed = std::mem::take(&mut self.baseline_dropped);
            if armed {
                if self.is_replay(&added) || !is_one_commit(&added) {
                    return self.desync();
                }
                self.dropped = None;
            }
        }

        if removed > MAX_SNAPSHOT_DELETE {
            return self.desync();
        }

        let mut backspaces = removed;
        if backspaces > 0 {
            let consumed = backspaces.min(self.pending_backspaces);
            backspaces -= consumed;
            self.pending_backspaces -= consumed;
        } else {
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

    /// No platform mirror on desktop: the mirror is untouched unless the commit carries a Return.
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

    pub fn on_composition_end(&mut self) -> Emission {
        if self.composing.take().is_some() {
            self.revision += 1;
        }
        Emission::inert(InputClass::None)
    }

    /// `Key::Named(Backspace)` arms the dedupe that swallows the platform's echo; Android's Return
    /// (`IME_ACTION_DONE`) drops the baseline as iOS's literal `"\n"` does.
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
                // Through the sequence path so a hardware Shift+Tab picks the bar's variant.
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

    /// `Fn` and an out-of-range `F(n)` derive nothing.
    pub fn on_bar_key(&mut self, key: BarKey) -> Emission {
        let latch = match key {
            BarKey::Ctrl => &mut self.latch.ctrl,
            BarKey::Alt => &mut self.latch.alt,
            BarKey::Shift => &mut self.latch.shift,
            _ => {
                let Some(sequence) = key.sequence() else {
                    return Emission::inert(InputClass::None);
                };
                let bytes = self.emit_sequence(sequence, ModifierLatch::NONE);
                return Emission {
                    bytes,
                    reconciled: false,
                    class: InputClass::Sequence,
                };
            }
        };
        *latch = !*latch;
        self.revision += 1;
        Emission::inert(InputClass::Latch)
    }
}

#[cfg(test)]
mod tests {
    use super::super::harness::{hex, snapshot, type_text};
    use super::*;

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

        // The platform collapsed two deletes into one snapshot.
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

        assert!(emission.reconciled, "the window re-anchors, to empty");
        assert_eq!(source.mirror(), "", "no `\\n` is left in the mirror");
        assert_eq!(source.buffered_units(), 0, "…and none in history either");
    }

    #[test]
    fn the_keystroke_after_a_return_diffs_against_an_empty_baseline() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        source.on_snapshot(&snapshot("ls\n"));

        // The platform reconciled to the empty mirror inside the same round trip.
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
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "hunter2");
        assert!(source.buffered_units() > 0);

        let emission = source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert!(emission.reconciled, "one restartInput, on the rarest key");
        assert_eq!(source.buffered_units(), 0);
        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn a_stale_snapshot_after_a_return_replays_nothing_to_the_host() {
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "ls")), "6c73");
        let submitted = source.on_snapshot(&snapshot("ls\n"));
        assert_eq!(hex(&submitted.bytes), "0d", "the Return went out");
        assert_eq!(source.mirror(), "", "…and guard 1 dropped the baseline");
        let sent = source.emitted();

        // The platform's stale buffer, not `source.mirror()`, which would hide the replay.
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
        // `IME_ACTION_DONE` leaves as an editor action, so Android's stale buffer has no newline.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "sudo -i")), "7375646f202d69");
        assert_eq!(
            hex(&source
                .on_key(&Key::Named(NamedKey::Enter), Modifiers::default())
                .bytes),
            "0d"
        );
        assert_eq!(source.mirror(), "", "…leaving an empty baseline, armed");
        let sent = source.emitted();

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
        // iOS keeps the literal Return in its stale buffer.
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

            let emission = source.on_snapshot(&snapshot(first));
            assert_eq!(hex(&emission.bytes), expected, "{first:?} must land");
            assert_eq!(emission.class, InputClass::Text);
            assert_eq!(source.mirror(), first, "…and become the new baseline");
        }
    }

    #[test]
    fn a_no_op_snapshot_does_not_spend_the_stale_shot() {
        // Gboard's `finishComposingText()` re-sends the mirror unchanged without spending the shot.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "ls")), "6c73");
        assert_eq!(hex(&source.on_snapshot(&snapshot("ls\n")).bytes), "0d");
        assert_eq!(source.mirror(), "", "the baseline was dropped");
        let sent = source.emitted();

        let no_op = source.on_snapshot(&snapshot(""));
        assert!(no_op.bytes.is_empty());
        assert_eq!(no_op.class, InputClass::None);
        assert_eq!(source.emitted(), sent, "a positive precondition: no-op");

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

        assert_eq!(
            hex(&type_text(&mut source, "x")),
            "78",
            "typing still lands"
        );
        let second = source.on_snapshot(&snapshot("x\n"));
        assert_eq!(hex(&second.bytes), "0d", "the next line still submits");
        assert_eq!(second.class, InputClass::Enter);
        let stale = source.on_snapshot(&snapshot("x\n"));
        assert!(stale.bytes.is_empty());
        assert_eq!(stale.class, InputClass::Desync);
    }

    #[test]
    fn a_return_and_nothing_else_is_let_through_after_a_return() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "ls");
        assert_eq!(hex(&source.on_snapshot(&snapshot("ls\n")).bytes), "0d");

        let blank = source.on_snapshot(&snapshot("\n"));
        assert_eq!(hex(&blank.bytes), "0d", "the blank line still submits");
        assert_eq!(blank.class, InputClass::Enter);
        assert_eq!(source.buffered_units(), 0, "and still keeps nothing");
    }

    #[test]
    fn a_second_stale_snapshot_in_the_same_window_replays_nothing_either() {
        // The same stale buffer can be polled on several frames.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "sudo -i")), "7375646f202d69");
        assert_eq!(
            hex(&source
                .on_key(&Key::Named(NamedKey::Enter), Modifiers::default())
                .bytes),
            "0d"
        );
        assert_eq!(source.mirror(), "", "armed, on an empty baseline");
        let sent = source.emitted();

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

        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn an_oversized_delete_desync_leaves_the_guard_armed_for_the_frame_after_it() {
        let mut source = KeystrokeSource::new();
        let typed = "a".repeat(MIRROR_CAP / 2);
        type_text(&mut source, &typed);
        let sent = source.emitted();
        assert_eq!(
            sent,
            typed.len(),
            "a positive precondition: it all went out"
        );

        let over = &typed[..typed.len() - (MAX_SNAPSHOT_DELETE + 1)];
        let refused = source.on_snapshot(&snapshot(over));
        assert_eq!(refused.class, InputClass::Desync);
        assert_eq!(source.mirror(), typed, "the baseline does not move");

        // A bulk add is what a bridge one frame behind reports.
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

        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn a_word_shaped_stale_line_is_refused_on_its_content_not_its_shape() {
        // By shape `"hunter2"` is one commit; only the content test refuses it.
        let mut source = KeystrokeSource::new();
        assert_eq!(hex(&type_text(&mut source, "hunter2")), "68756e74657232");
        assert_eq!(
            hex(&source
                .on_key(&Key::Named(NamedKey::Enter), Modifiers::default())
                .bytes),
            "0d"
        );
        assert_eq!(source.mirror(), "", "armed, on an empty baseline");
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
        // Refused whole rather than emitting the `x` alone: a fragment of a keystroke run is worse
        // than a dropped one.
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

        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn a_genuine_gesture_word_after_a_word_shaped_line_still_lands() {
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
        // Lengths match and digests do not: the seed is a per-instance `RandomState`.
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
        for probe in ["", "a", "ls -la", "hunter2x"] {
            assert_ne!(
                first.digest(probe),
                second.digest(probe),
                "{probe:?} digests the same in both sources"
            );
        }
        assert_eq!(first.digest(line), first_digest);
        assert_eq!(first.buffered_units(), 0);
        assert_eq!(first.published().text, "");
    }

    #[test]
    fn a_one_keystroke_baseline_records_no_digest_and_stays_typeable() {
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

        // A digest this short would make `y` at a `[y/n]` prompt untypeable: a refusal re-arms.
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "y");
        source.on_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert_eq!(hex(&type_text(&mut source, "y")), "79", "`y` still types");
    }

    #[test]
    fn a_blur_arms_the_same_shot_a_return_does() {
        let mut source = KeystrokeSource::new();
        type_text(&mut source, "secret");
        source.scrub();

        let stale = source.on_snapshot(&snapshot("secret\n"));
        assert!(stale.bytes.is_empty(), "no replay after a blur either");
        assert_eq!(stale.class, InputClass::Desync);
    }

    #[test]
    fn a_whole_buffer_replacement_emits_nothing_and_re_publishes() {
        // Dictation, a replace-span rewrite or a lagging bridge: one snapshot deleting the buffer.
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

        let keep = typed.len() - MAX_SNAPSHOT_DELETE;
        let emission = source.on_snapshot(&snapshot(&typed[..keep]));
        assert_eq!(emission.bytes.len(), MAX_SNAPSHOT_DELETE);
        assert_eq!(emission.class, InputClass::Backspace);

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
        assert_eq!(hex(&type_text(&mut source, "z")), "7a");
    }

    #[test]
    fn the_composing_echo_is_retained_typing_and_is_scrubbed_as_such() {
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
        let mut source = KeystrokeSource::new();
        source.on_compose("nih");
        assert_eq!(source.published().text, "nih");
        assert_eq!(source.published().composing_extent, 3);

        source.on_compose("n");
        assert_eq!(source.published().text, "n", "no stale tail survives");
        assert_eq!(source.published().composing_extent, 1);
        assert_eq!(source.buffered_units(), 1);

        source.on_composition_end();
        assert_eq!(
            hex(&source.on_snapshot(&snapshot("\u{4f60}")).bytes),
            "e4bda0"
        );
    }

    #[test]
    fn a_key_backspace_and_its_echoed_shortening_emit_exactly_one_byte() {
        // Android's shape: the key event and the mirror shortening.
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
        assert_eq!(hex(&source.on_snapshot(&snapshot("abc")).bytes), "63");
        assert_eq!(hex(&source.on_snapshot(&snapshot("ab")).bytes), "7f");
    }

    #[test]
    fn a_surrogate_pair_emits_its_utf8_and_is_never_split() {
        let mut source = KeystrokeSource::new();
        let emission = source.on_snapshot(&snapshot("a\u{1f600}"));
        assert_eq!(hex(&emission.bytes), "61f09f9880");
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

        let committed = source.on_snapshot(&snapshot("a\u{4f60}"));
        assert_eq!(hex(&committed.bytes), "e4bda0");
    }

    #[test]
    fn a_composing_range_is_validated_before_it_is_echoed_back() {
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
        // Called directly: the re-anchor after it would hide the oversized store.
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

        assert!(source.reanchor_if_needed(), "the window still re-anchors");
        assert_eq!(len_utf16(source.mirror()), MIRROR_TAIL);
        assert_eq!(hex(&type_text(&mut source, "x")), "78");
    }

    #[test]
    fn an_oversized_paste_emits_every_byte_before_it_is_clamped_away() {
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
        // One unit past the cap, so the trim is the last thing to fire.
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

        assert!(!source.latch().armed());
        let plain = source.on_key(&Key::Named(NamedKey::ArrowUp), Modifiers::default());
        assert_eq!(hex(&plain.bytes), "1b5b41");

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

    #[test]
    fn a_hardware_f_key_takes_its_modified_form_from_its_plain_bytes() {
        let mods = |shift, alt, ctrl| Modifiers {
            shift,
            alt,
            ctrl,
            ..Modifiers::default()
        };
        let mut source = KeystrokeSource::new();
        let f5 = Key::Named(NamedKey::F5);
        let cases: [(Modifiers, &[u8]); 3] = [
            (mods(true, false, false), b"\x1b[15;2~"),
            (mods(false, true, false), b"\x1b[15;3~"),
            (mods(false, false, true), b"\x1b[15;5~"),
        ];
        for (m, expected) in cases {
            assert_eq!(source.on_key(&f5, m).bytes, expected);
        }
        let f1 = Key::Named(NamedKey::F1);
        assert_eq!(
            source.on_key(&f1, Modifiers::default()).bytes,
            b"\x1bOP".to_vec()
        );
        assert_eq!(
            source.on_key(&f1, mods(false, false, true)).bytes,
            b"\x1b[1;5P".to_vec()
        );
    }
}
