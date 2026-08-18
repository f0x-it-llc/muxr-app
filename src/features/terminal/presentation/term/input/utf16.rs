//! UTF-16 arithmetic — the seam's index unit — and the mirror diff built on it.
//! Every index at the IME seam is a UTF-16 code unit (frust's
//! `docs/CODE_STANDARDS.md`: UTF-16 at the FFI seam, bytes inside). The rounding
//! direction of each slice is part of its contract: outward never splits a surrogate
//! pair, inward never returns half of one.

/// `s`'s length in **UTF-16 code units**. Deliberately never `s.len()`:
/// `"\u{1f600}"` is 4 bytes, 1 char and 2 units, and a byte-length diff is the first
/// bug this pattern invites.
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

/// The first `units` UTF-16 code units of `s`, rounded **inward** to a character
/// boundary so a surrogate pair is never split. Inward is the safe direction for its
/// one caller (`KeystrokeSource::is_replay`): the dropped baseline it is compared
/// against is whole characters, so a prefix that would have to split a pair is not
/// that baseline — and coming up **short** makes the caller's own length check
/// reject it, rather than digesting a slice that is one unit off.
pub(super) fn utf16_prefix(s: &str, units: usize) -> &str {
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

/// Drop leading characters from `s` until at most `units` UTF-16 units long,
/// character-aligned — [`utf16_suffix`] as an in-place truncation. **In
/// place, deliberately**: `*s = utf16_suffix(s, units).to_string()` would
/// leave the user's typing in a freed allocation nobody scrubs. `drain`
/// shifts the tail down in place, so the only copy of the dropped head is
/// stale bytes past the new length, zeroed (with the rest) by
/// `scrub`/`Zeroizing`'s drop across the buffer's **whole capacity**.
pub(super) fn clamp_front(s: &mut String, units: usize) {
    if len_utf16(s) <= units {
        return;
    }
    let start = s.len() - utf16_suffix(s, units).len();
    s.drain(..start);
}

/// One index at the seam, clamped into a `len`-unit buffer: a negative index
/// normalizes to the `-1` "none" sentinel `EditingState` documents, and a
/// positive one can never point past the end.
pub(super) fn clamp_index(index: i32, len: i32) -> i32 {
    if index < 0 { -1 } else { index.min(len) }
}

/// The minimal `(removed, added)` edit turning `old` into `new`: common
/// prefix/suffix cancelled, `removed` counting the UTF-16 units the middle of
/// `old` held, `added` the middle of `new`. Both boundaries are
/// **character**-aligned, so a surrogate pair can never split across a diff
/// boundary. `added` is a fresh allocation holding the user's own text:
/// `on_snapshot` takes ownership inside a `Zeroizing` so its drop scrubs, and
/// any other caller must too.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
