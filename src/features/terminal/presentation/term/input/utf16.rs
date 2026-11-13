pub fn len_utf16(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Rounds outward to a character boundary, so a surrogate pair is never split.
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

/// Rounds inward, so a split pair comes up short and fails the caller's length check.
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

/// In place, so the dropped head stays in the buffer `scrub` zeroes rather than a freed allocation.
pub(super) fn clamp_front(s: &mut String, units: usize) {
    if len_utf16(s) <= units {
        return;
    }
    let start = s.len() - utf16_suffix(s, units).len();
    s.drain(..start);
}

/// A negative index becomes the `-1` "none" sentinel; a positive one never exceeds `len`.
pub(super) fn clamp_index(index: i32, len: i32) -> i32 {
    if index < 0 { -1 } else { index.min(len) }
}

/// Character-aligned `(removed UTF-16 units, added)`; `added` is user text for a `Zeroizing`.
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
        assert_eq!(utf16_suffix("ab\u{1f600}", 1), "\u{1f600}");
        assert_eq!(utf16_suffix("ab\u{1f600}", 2), "\u{1f600}");
        assert_eq!(utf16_suffix("ab\u{1f600}", 3), "b\u{1f600}");
    }

    #[test]
    fn utf16_prefix_rounds_inward_and_never_splits_a_pair() {
        assert_eq!(utf16_prefix("abcdef", 3), "abc");
        assert_eq!(utf16_prefix("abc", 9), "abc");
        assert_eq!(utf16_prefix("abc", 0), "");
        // One unit of a leading pair yields nothing: the inward rounding.
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
        assert_eq!(diff("a\u{1f600}", "a"), (2, String::new()));
    }
}
