//! Comment-residue ratchet: a source-scan test locking `docs/CODE_STANDARDS.md`'s (the frust
//! checkout's) Comment Conventions § shut for this app. A slimmed port of frust's own
//! `crates/frust/tests/comment_residue_conformance.rs` — same scanning mechanics, narrowed to
//! this app's two source roots (`src/`, `tests/`; this file itself excluded, see [`self_path`])
//! plus one muxr-specific family the frust ratchet doesn't need: gate/G-ledger citations
//! (`gate-rN`, standalone `G##`) — the device-gate review-round shorthand this repo's own
//! commits use (e.g. `(gate-r9, G53)`).
//!
//! Comment-only, substring scan — no parser: for each line, the text from its first `//`
//! onward, unless that `//` sits after an odd `"` count (a string-literal heuristic — the whole
//! line is then invisible to this scan). A citation wrapped across a `//` line break (`(review`
//! / `finding M1)`) is caught by also scanning each comment joined, pairwise, to a comment-only
//! continuation line ([`continued_comment`]) — reported once, at the line the citation starts
//! on, only when the next line isn't already a per-line failure on its own
//! ([`wrapped_violation`]). Banned-pattern detection ([`banned_hits`]) yields byte spans; the
//! allowlist ([`sanctioned_spans`]) exempts only a match's own span, not the whole line.
//!
//! Muxr has no `docs/LIMITATIONS.md`, so the reference's LIMITATIONS-id allowlist class is
//! dropped here. Its other two sanctioned forms stay, both with real citations already in this
//! tree: named semantic rules (`` R44-back ``, cited from `src/lib.rs`) and external-repo rev
//! pins (`clean-signals`' `` rev `910f626` ``, per `Cargo.toml`'s dependency comment).
//!
//! # Gate/G-ledger precision ([`gate_hits`])
//!
//! `gate-rN` is unambiguous — no legitimate word starts `gate-r` followed by digits — and is
//! always banned. A bare `G` plus 1-3 digits is not: this app embeds the `vt100` crate, whose
//! own escape-sequence vocabulary designates character-set slots `G0`-`G3` (`shift-in`/
//! `shift-out` select between them) — a real "legitimate identifier" collision a naive `G\d+`
//! pattern would flag on sight. A standalone `G##` is therefore a hit only with a supporting
//! signal: the word `gate` within [`GATE_WINDOW`] bytes before or after it
//! ([`window_has_gate`], word-bounded so `gateway`/`delegate` never count but the `gate-r9` in
//! `(gate-r9, G53)` does), or direct adjacency to another `G##` via a hyphenated range/list
//! ([`adjacent_g_range`], e.g. `G44-G54`). Neither signal fires on a bare `G0`/`G1` charset
//! citation standing alone.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// This crate's own root — `tests/`/`src/` both hang directly off it (unlike the frust
/// workspace reference, this app's manifest dir *is* its scan root).
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// This test's own file — excluded from its own scan.
fn self_path() -> PathBuf {
    crate_root().join("tests/comment_residue.rs")
}

/// The two roots this scan walks (not, e.g., `android/`, `ios/`, `build.rs`).
const SCAN_ROOTS: &[&str] = &["src", "tests"];

fn is_excluded_dir_component(name: &str) -> bool {
    name == "target"
}

/// Recursively collects every `.rs` file under `dir` into `out`, skipping `target/`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("dir entry under {}: {e}", dir.display()))
            .path();
        if path.is_dir() {
            let excluded = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_excluded_dir_component);
            if excluded {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every `.rs` file across [`SCAN_ROOTS`], minus this test's own file, sorted for a stable
/// failure order. Panics rather than silently scanning zero files.
fn scan_source_files() -> Vec<PathBuf> {
    let root = crate_root();
    let me = self_path();
    let mut out = Vec::new();
    for scan_root in SCAN_ROOTS {
        let dir = root.join(scan_root);
        assert!(
            dir.is_dir(),
            "expected scan root {} to exist",
            dir.display()
        );
        let before = out.len();
        rust_files(&dir, &mut out);
        assert!(
            out.len() > before,
            "expected at least one `.rs` file under {} — the false-confidence failure mode this \
             assertion exists to catch",
            dir.display()
        );
    }
    out.retain(|p| p != &me);
    out.sort();
    assert!(
        out.len() > 100,
        "expected well over 100 `.rs` files across src/tests, found {} — scan looking in the \
         wrong place?",
        out.len()
    );
    out
}

/// `path` relative to the crate root, forward-slashed.
fn rel(path: &Path) -> String {
    path.strip_prefix(crate_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The comment-text substring of `line` (from its first `//` onward), or `None` if there is no
/// `//`, or its first `//` sits inside a string literal by the odd-quote heuristic.
fn comment_text(line: &str) -> Option<&str> {
    let idx = line.find("//")?;
    let quotes_before = line[..idx].matches('"').count();
    if quotes_before % 2 == 1 {
        return None;
    }
    Some(&line[idx..])
}

/// The prose body of a comment text: its `//`/`///`/`//!` marker stripped, indentation trimmed.
fn comment_body(comment: &str) -> &str {
    let rest = comment.strip_prefix("//").unwrap_or(comment);
    let rest = rest
        .strip_prefix('/')
        .or_else(|| rest.strip_prefix('!'))
        .unwrap_or(rest);
    rest.trim_start()
}

/// `comment` joined to `next_line`'s comment text, marker stripped — the logical comment a
/// wrapped citation lives in. `None` unless `next_line` is a comment-ONLY, non-empty-body
/// continuation (code, a blank line, or a bare `//` spacer all end the run).
fn continued_comment(comment: &str, next_line: &str) -> Option<String> {
    if !next_line.trim_start().starts_with("//") {
        return None;
    }
    let tail = comment_body(comment_text(next_line)?);
    if comment_body(comment).is_empty() || tail.is_empty() {
        return None;
    }
    Some(format!("{} {tail}", comment.trim_end()))
}

/// The residue a citation hides by wrapping onto `next_line`. Reported only when `next_line` is
/// clean on its own — a per-line failure there is not double-reported here.
fn wrapped_violation(comment: &str, next_line: &str) -> Option<&'static str> {
    let joined = continued_comment(comment, next_line)?;
    if scan_comment(comment_text(next_line)?).is_some() {
        return None;
    }
    scan_comment(&joined)
}

/// Every starting byte offset of `needle` in `haystack` (non-overlapping, left to right).
fn find_all(haystack: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel_idx) = haystack.get(from..).and_then(|s| s.find(needle)) {
        let idx = from + rel_idx;
        out.push(idx);
        from = idx + needle.len().max(1);
    }
    out
}

fn leading_digit_run(s: &str) -> usize {
    s.chars().take_while(char::is_ascii_digit).count()
}

/// True if byte offset `idx` in `s` is preceded by a word boundary.
fn word_boundary_before(s: &str, idx: usize) -> bool {
    idx == 0 || {
        let b = s.as_bytes()[idx - 1];
        !(b.is_ascii_alphanumeric() || b == b'_')
    }
}

/// True if `s` (the text immediately following a match) starts at a word boundary.
fn word_boundary_after(s: &str) -> bool {
    s.chars()
        .next()
        .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
}

/// The lowercased word immediately before byte offset `idx` in `s`, or `""` if nothing
/// word-shaped precedes it.
fn preceding_word(s: &str, idx: usize) -> String {
    let before = s[..idx].trim_end();
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '\'' || *c == '-')
        .last()
        .map_or(before.len(), |(i, _)| i);
    before[start..].to_ascii_lowercase()
}

/// Up to `max_len` bytes of `s` starting at `start`, truncated to a char boundary.
fn window_after(s: &str, start: usize, max_len: usize) -> &str {
    let mut end = start.saturating_add(max_len).min(s.len());
    while end > start && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[start..end]
}

/// Up to `max_len` bytes of `s` ending at `end` (exclusive) — [`window_after`]'s mirror.
fn window_before(s: &str, end: usize, max_len: usize) -> &str {
    let mut start = end.saturating_sub(max_len);
    while start < end && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..end]
}

/// True if `window` contains any of `words` as a whole token (split on any char that isn't
/// ASCII-alphanumeric or `-`, so `busy-spin` is one token).
fn window_has_word(window: &str, words: &[&str]) -> bool {
    window
        .to_ascii_lowercase()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .any(|token| words.contains(&token.trim_matches('-')))
}

/// True if `window` contains `gate` at its own word boundary: matches bare `gate` and the
/// `gate` prefix of `gate-r9` (boundary after is the hyphen), never `gateway`/`delegate`
/// (letters after/before break the boundary). Not [`window_has_word`]: that folds a hyphen into
/// its token, so `gate-r9` would never equal the bare token `gate`.
fn window_has_gate(window: &str) -> bool {
    let lower = window.to_ascii_lowercase();
    find_all(&lower, "gate")
        .into_iter()
        .any(|idx| word_boundary_before(window, idx) && word_boundary_after(&window[idx + 4..]))
}

/// One banned-pattern match: the byte range it occupies (the unit the allowlist exempts) plus
/// the label reported when it isn't exempt.
struct Hit {
    range: Range<usize>,
    reason: &'static str,
}

const PHASE_REASON: &str = "plan-phase reference (`Phase 9.B step 1`/`(phase 5.5)`/hyphenated \
     `phase-7`)";
const TASK_REASON: &str = "plan-task reference (`task-12`/`task g3`)";
const PLAN_DOC_REASON: &str = "plan-document reference (`PLAN D6a`/`Plan D5 §Requirements`)";
const PLAN_PATH_REASON: &str = "workflow plan path (`workflow/plans/...`)";
const FINDINGS_REASON: &str = "workflow findings ledger number (`FINDINGS #43`)";
const REVIEW_FINDING_REASON: &str = "review-finding citation (`(review finding F5)`)";
const ROUND_REASON: &str = "review-round reference (`the round-1 busy-spin Critical`)";
const CFIX_REASON: &str = "fix-round reference (`cfix-2`)";
const RE_REVIEW_REASON: &str = "review-round reference (`re-review`)";
const REVIEW_ROUND_REASON: &str = "review-round reference (`review round N`)";
const DEVICE_PARITY_REASON: &str = "device-parity-round reference";
const REQ_REASON: &str = "plan requirement number (`req N`) alongside a phase reference";
const GATE_REASON: &str = "gate/G-ledger reference (`gate-r9`/`G53`)";

/// Pushes every banned plan-phase reference (dotted, hyphenated, or parenthesized/
/// article-labelled bare int). `Phase N of the frame` (bare), an un-parenthesized pure float
/// (the animation/oscillator idiom), and a lowercase parenthesized bare int (the
/// animation-rest-state idiom) are allowlisted by construction — simply never matched.
fn phase_hits(comment: &str, out: &mut Vec<Hit>) {
    let lower = comment.to_ascii_lowercase();
    let mut search_from = 0usize;
    while let Some(rel_idx) = lower.get(search_from..).and_then(|s| s.find("phase")) {
        let idx = search_from + rel_idx;
        search_from = idx + 5;

        if !word_boundary_before(comment, idx) {
            continue;
        }
        let capitalized = comment.as_bytes()[idx] == b'P';
        let parenthesized = idx > 0 && comment.as_bytes()[idx - 1] == b'(';

        let after = &comment[idx + 5..];
        let Some(sep) = after.chars().next() else {
            continue;
        };
        if sep != '-' && sep != ' ' {
            continue;
        }
        let hyphenated = sep == '-';
        let rest = &after[sep.len_utf8()..];
        let digit_len = leading_digit_run(rest);
        if digit_len == 0 {
            continue;
        }
        let bare_end = idx + 5 + sep.len_utf8() + digit_len;
        let after_digits = &rest[digit_len..];

        if let Some(suffix) = after_digits.strip_prefix('.') {
            let suffix_len = suffix
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .count();
            if suffix_len == 0 {
                continue;
            }
            let has_letter = suffix[..suffix_len]
                .chars()
                .any(|c| c.is_ascii_alphabetic());
            if has_letter || parenthesized {
                // digit.LETTER: always a plan-phase id. Parenthesized pure-float:
                // `docs/CODE_STANDARDS.md`'s own `(phase 5.5)` example. Un-parenthesized pure
                // float (neither branch): the animation/oscillator idiom, allowlisted.
                out.push(Hit {
                    range: idx..bare_end + 1 + suffix_len,
                    reason: PHASE_REASON,
                });
            }
        } else if hyphenated {
            // Hyphenated bare int: this repo's plan-label spelling, any case/position.
            out.push(Hit {
                range: idx..bare_end,
                reason: PHASE_REASON,
            });
        } else if (parenthesized && capitalized)
            || (capitalized && preceding_word(comment, idx) == "the")
        {
            // Parenthesized+capitalized, or a definite article before it: a plan-phase
            // citation, never the renderer-span/scenario-stage idiom.
            out.push(Hit {
                range: idx..bare_end,
                reason: PHASE_REASON,
            });
        }
    }
}

/// Pushes every `task-NN`/`task NN` (exactly two digits) and `task gN` (exactly one digit) —
/// lowercase `task` only.
fn task_hits(comment: &str, out: &mut Vec<Hit>) {
    for idx in find_all(comment, "task") {
        if !word_boundary_before(comment, idx) {
            continue;
        }
        let after = &comment[idx + 4..];
        let Some(sep) = after.chars().next() else {
            continue;
        };
        if sep != '-' && sep != ' ' {
            continue;
        }
        let rest = &after[sep.len_utf8()..];
        if leading_digit_run(rest) == 2 && word_boundary_after(&rest[2..]) {
            out.push(Hit {
                range: idx..idx + 4 + sep.len_utf8() + 2,
                reason: TASK_REASON,
            });
            continue;
        }
        if sep == ' '
            && rest.starts_with('g')
            && leading_digit_run(&rest[1..]) == 1
            && word_boundary_after(&rest[2..])
        {
            out.push(Hit {
                range: idx..idx + 4 + 1 + 2,
                reason: TASK_REASON,
            });
        }
    }
}

/// `Some(len)` if `s` starts with a plan-document tag: an uppercase letter, digits, an optional
/// lowercase suffix, then a word boundary (`D6a`, `D5`).
fn plan_tag_len(s: &str) -> Option<usize> {
    if !s.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    let digits = leading_digit_run(&s[1..]);
    if digits == 0 {
        return None;
    }
    let tail = &s[1 + digits..];
    let letters = tail.chars().take_while(char::is_ascii_lowercase).count();
    if !word_boundary_after(&tail[letters..]) {
        return None;
    }
    Some(1 + digits + letters)
}

/// Pushes every `PLAN <tag>`/`Plan <tag>` and any literal `workflow/plans` path.
fn plan_doc_hits(comment: &str, out: &mut Vec<Hit>) {
    let lower = comment.to_ascii_lowercase();
    for idx in find_all(&lower, "plan ") {
        if !word_boundary_before(comment, idx) {
            continue;
        }
        let Some(tag_len) = plan_tag_len(&comment[idx + 5..]) else {
            continue;
        };
        out.push(Hit {
            range: idx..idx + 5 + tag_len,
            reason: PLAN_DOC_REASON,
        });
    }
    for idx in find_all(&lower, "workflow/plans") {
        out.push(Hit {
            range: idx..idx + "workflow/plans".len(),
            reason: PLAN_PATH_REASON,
        });
    }
}

/// Pushes every `FINDINGS #N` (all-caps plural) and `[Ff]inding #N` (singular) citation.
fn findings_hits(comment: &str, out: &mut Vec<Hit>) {
    for idx in find_all(comment, "FINDINGS #") {
        let digits = leading_digit_run(&comment[idx + "FINDINGS #".len()..]);
        if digits > 0 {
            out.push(Hit {
                range: idx..idx + "FINDINGS #".len() + digits,
                reason: FINDINGS_REASON,
            });
        }
    }
    for idx in find_all(comment, "inding #") {
        if idx == 0 || !matches!(comment.as_bytes()[idx - 1], b'F' | b'f') {
            continue;
        }
        let digits = leading_digit_run(&comment[idx + "inding #".len()..]);
        if digits > 0 {
            out.push(Hit {
                range: idx - 1..idx + "inding #".len() + digits,
                reason: FINDINGS_REASON,
            });
        }
    }
}

/// `Some(len)` if `s` starts with ` ` plus a review-finding id (` F5`).
fn review_finding_id_len(s: &str) -> Option<usize> {
    let rest = s.strip_prefix(' ')?;
    if !rest.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    let digits = leading_digit_run(&rest[1..]);
    if digits == 0 || !word_boundary_after(&rest[1 + digits..]) {
        return None;
    }
    Some(1 + 1 + digits)
}

/// Pushes every review-finding citation: `review finding` either directly after `(` or directly
/// followed by a finding id. Prose about review findings with neither marker is kept.
fn review_finding_hits(comment: &str, out: &mut Vec<Hit>) {
    let lower = comment.to_ascii_lowercase();
    const NEEDLE: &str = "review finding";
    for idx in find_all(&lower, NEEDLE) {
        if !word_boundary_before(comment, idx) {
            continue;
        }
        let end = idx + NEEDLE.len();
        let parenthesized = idx > 0 && comment.as_bytes()[idx - 1] == b'(';
        let id_len = review_finding_id_len(&comment[end..]).unwrap_or(0);
        if parenthesized || id_len > 0 {
            out.push(Hit {
                range: idx..end + id_len,
                reason: REVIEW_FINDING_REASON,
            });
        }
    }
}

/// How far past a `round-N` label [`round_hits`] looks for review vocabulary.
const ROUND_WINDOW: usize = 48;

/// Determiners that, immediately before `round-N`, mark it as a specific-round label.
const ROUND_DETERMINERS: &[&str] = &[
    "the", "other", "another", "this", "that", "its", "first", "second", "third",
];

/// Review vocabulary that, within [`ROUND_WINDOW`] bytes after `round-N`, marks it as a
/// review-round label.
const ROUND_REVIEW_WORDS: &[&str] = &[
    "bug",
    "bugs",
    "critical",
    "criticals",
    "review",
    "reviews",
    "fix",
    "fixes",
    "regression",
    "regressions",
    "busy-spin",
    "case",
    "cases",
    "finding",
    "findings",
    "major",
    "majors",
    "minor",
    "minors",
];

/// Pushes every `round-N` review-round label — a determiner before it or review vocabulary
/// after — so `round-trip`, `rounds to zero`, and `rounding` can never match.
fn round_hits(comment: &str, out: &mut Vec<Hit>) {
    let lower = comment.to_ascii_lowercase();
    for idx in find_all(&lower, "round-") {
        if !word_boundary_before(comment, idx) {
            continue;
        }
        let rest = &comment[idx + "round-".len()..];
        let digits = leading_digit_run(rest);
        if digits == 0 || !word_boundary_after(&rest[digits..]) {
            continue;
        }
        let end = idx + "round-".len() + digits;
        let determiner = ROUND_DETERMINERS.contains(&preceding_word(comment, idx).as_str());
        let vocabulary =
            window_has_word(window_after(comment, end, ROUND_WINDOW), ROUND_REVIEW_WORDS);
        if determiner || vocabulary {
            out.push(Hit {
                range: idx..end,
                reason: ROUND_REASON,
            });
        }
    }
}

fn digit_suffixed_hits(comment: &str, needle: &str, reason: &'static str, out: &mut Vec<Hit>) {
    for idx in find_all(comment, needle) {
        let digits = leading_digit_run(&comment[idx + needle.len()..]);
        if digits > 0 {
            out.push(Hit {
                range: idx..idx + needle.len() + digits,
                reason,
            });
        }
    }
}

fn literal_hits(comment: &str, needle: &str, reason: &'static str, out: &mut Vec<Hit>) {
    for idx in find_all(comment, needle) {
        out.push(Hit {
            range: idx..idx + needle.len(),
            reason,
        });
    }
}

/// Pushes every `req N` match, but only when the comment also contains `phase` — a bare `req`
/// is too collision-prone on its own.
fn req_with_phase_hits(comment: &str, out: &mut Vec<Hit>) {
    if !comment.to_ascii_lowercase().contains("phase") {
        return;
    }
    for idx in find_all(comment, "req ") {
        if !word_boundary_before(comment, idx) {
            continue;
        }
        let digits = leading_digit_run(&comment[idx + 4..]);
        if digits > 0 {
            out.push(Hit {
                range: idx..idx + 4 + digits,
                reason: REQ_REASON,
            });
        }
    }
}

/// How far [`gate_hits`] looks, before or after a standalone `G##`, for the word `gate`.
const GATE_WINDOW: usize = 40;

/// True if the `G##` spanning `comment[idx..end]` sits directly beside another `G##` via a
/// hyphenated range/list (`G44-G54`): a digit-then-hyphen just before `idx`, or a hyphen-`G`-
/// digit just after `end`.
fn adjacent_g_range(comment: &str, idx: usize, end: usize) -> bool {
    let before_range = idx >= 2
        && comment.as_bytes()[idx - 1] == b'-'
        && comment.as_bytes()[idx - 2].is_ascii_digit();
    let after = &comment[end..];
    let after_range =
        after.starts_with('-') && after[1..].starts_with('G') && leading_digit_run(&after[2..]) > 0;
    before_range || after_range
}

/// Pushes every `gate-rN` (always banned) and every standalone `G##` carrying a supporting
/// signal (see module doc's Gate/G-ledger precision §). A bare `G0`-`G3` (this app's `vt100`
/// charset-slot vocabulary) with neither signal is never flagged.
fn gate_hits(comment: &str, out: &mut Vec<Hit>) {
    let lower = comment.to_ascii_lowercase();
    for idx in find_all(&lower, "gate-r") {
        if !word_boundary_before(comment, idx) {
            continue;
        }
        let digits = leading_digit_run(&comment[idx + "gate-r".len()..]);
        if digits == 0 || !word_boundary_after(&comment[idx + "gate-r".len() + digits..]) {
            continue;
        }
        out.push(Hit {
            range: idx..idx + "gate-r".len() + digits,
            reason: GATE_REASON,
        });
    }

    for idx in find_all(comment, "G") {
        if !word_boundary_before(comment, idx) {
            continue;
        }
        let rest = &comment[idx + 1..];
        let digits = leading_digit_run(rest);
        if digits == 0 || digits > 3 || !word_boundary_after(&rest[digits..]) {
            continue;
        }
        let end = idx + 1 + digits;
        let signal = window_has_gate(window_before(comment, idx, GATE_WINDOW))
            || window_has_gate(window_after(comment, end, GATE_WINDOW))
            || adjacent_g_range(comment, idx, end);
        if signal {
            out.push(Hit {
                range: idx..end,
                reason: GATE_REASON,
            });
        }
    }
}

/// Every banned-pattern match in `comment`, sorted by position.
fn banned_hits(comment: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    phase_hits(comment, &mut hits);
    task_hits(comment, &mut hits);
    plan_doc_hits(comment, &mut hits);
    findings_hits(comment, &mut hits);
    review_finding_hits(comment, &mut hits);
    round_hits(comment, &mut hits);
    digit_suffixed_hits(comment, "cfix-", CFIX_REASON, &mut hits);
    literal_hits(comment, "re-review", RE_REVIEW_REASON, &mut hits);
    digit_suffixed_hits(comment, "review round ", REVIEW_ROUND_REASON, &mut hits);
    literal_hits(
        comment,
        "device-parity-round",
        DEVICE_PARITY_REASON,
        &mut hits,
    );
    req_with_phase_hits(comment, &mut hits);
    gate_hits(comment, &mut hits);
    hits.sort_by_key(|hit| (hit.range.start, hit.range.end));
    hits
}

/// The span of every named-semantic-rule citation (`R23`, `R44-back`): a word-boundary `R`,
/// digits, and any word-ish tail.
fn named_rule_spans(comment: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    for (i, byte) in comment.as_bytes().iter().enumerate() {
        if *byte != b'R' || !word_boundary_before(comment, i) {
            continue;
        }
        let rest = &comment[i + 1..];
        if leading_digit_run(rest) == 0 {
            continue;
        }
        let tail = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .map(char::len_utf8)
            .sum::<usize>();
        spans.push(i..i + 1 + tail);
    }
    spans
}

/// The span of every external-repo SHA citation in a `rev`/pin context (`` rev `910f626` ``).
fn external_rev_spans(comment: &str) -> Vec<Range<usize>> {
    let lower = comment.to_ascii_lowercase();
    let mut spans = Vec::new();
    for idx in find_all(&lower, "rev ") {
        let after = &comment[idx + 4..];
        let quotes = after.len() - after.trim_start_matches(['`', '"', '\'']).len();
        let hex = after[quotes..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .count();
        if (6..=40).contains(&hex) {
            let closer = usize::from(after[quotes + hex..].starts_with(['`', '"', '\'']));
            spans.push(idx..idx + 4 + quotes + hex + closer);
        }
    }
    spans
}

/// Every sanctioned-citation span in `comment` — the two KEEP classes muxr uses (see module
/// doc's Mechanics §; the reference's third class, `docs/LIMITATIONS.md` ids, has no analog
/// here — this app has no such register).
fn sanctioned_spans(comment: &str) -> Vec<Range<usize>> {
    let mut spans = named_rule_spans(comment);
    spans.extend(external_rev_spans(comment));
    spans
}

/// The full per-comment decision: `None` if clean (no banned pattern, or every match sits
/// inside a sanctioned span), `Some(reason)` for the first match that doesn't.
fn scan_comment(comment: &str) -> Option<&'static str> {
    let sanctioned = sanctioned_spans(comment);
    banned_hits(comment)
        .into_iter()
        .find(|hit| {
            !sanctioned
                .iter()
                .any(|span| span.start <= hit.range.start && hit.range.end <= span.end)
        })
        .map(|hit| hit.reason)
}

/// All violations in `contents` (one file's full text), `path:line: <reason> — see
/// docs/CODE_STANDARDS.md § Comment Conventions. Line: <text>`. A wrapped citation reports at
/// the line it starts on and prints both lines.
fn violations_in(path: &Path, contents: &str) -> Vec<String> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut failures = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(comment) = comment_text(line) else {
            continue;
        };
        let location = format!("{}:{}", rel(path), i + 1);
        if let Some(reason) = scan_comment(comment) {
            failures.push(format!(
                "{location}: {reason} — see docs/CODE_STANDARDS.md § Comment Conventions. Line: {}",
                line.trim(),
            ));
            continue;
        }
        let Some(next) = lines.get(i + 1) else {
            continue;
        };
        if let Some(reason) = wrapped_violation(comment, next) {
            failures.push(format!(
                "{location}: {reason}, wrapped across the line break — see \
                 docs/CODE_STANDARDS.md § Comment Conventions. Lines: {} | {}",
                line.trim(),
                next.trim(),
            ));
        }
    }
    failures
}

#[test]
fn sources_are_comment_residue_free() {
    let mut failures = Vec::new();
    for path in scan_source_files() {
        let contents =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        failures.extend(violations_in(&path, &contents));
    }
    assert!(
        failures.is_empty(),
        "comment-residue ban violated ({} hit(s)) — see docs/CODE_STANDARDS.md's Comment \
         Conventions §:\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// Fixture-driven unit tests. Most pattern/allowlist coverage is table-driven ([`keep_and_strip`]
/// KEEP/STRIP exemplars); a handful of targeted tests below cover mechanics the table can't
/// express (span-not-line exemption, wrapped-citation joining, byte-safety).
#[cfg(test)]
mod scan_behavior {
    use super::*;

    /// One assertion per exemplar rather than one test per pattern family — same shape as the
    /// frust reference's own `scanner_flags_and_allows_the_documented_exemplars`.
    #[test]
    fn keep_and_strip_exemplars() {
        let keep = [
            "Phase 1 of the frame — the **encode** span",
            "At clock phase 0.0, dot 0 is at local phase 0.0",
            "At rest (phase 0) spoke 0 is the leading one",
            "a round-trip through the encoder",
            "the accumulator rounds to zero",
            "a round-2 corner radius in device pixels",
            "a compile error rather than a review finding",
            "we plan to revisit this once the atlas lands",
            "the fallback plan B keeps the frame",
            "req 4 alone on its own line, nothing else",
            "tasked with something",
            "wave task g10",
            "task-123 has three digits, not two",
            "shift-in selects the G0 charset slot",
            "G1 is the alternate charset, per the vt100 spec",
            "the G20 finance ministers' communique",
            "the gatehouse-r9 fixture",
            "the R23 focus rule, still honored",
        ];
        for example in keep {
            assert_eq!(scan_comment(example), None, "expected KEEP {example:?}");
        }

        let strip = [
            "(Phase 9.B step 1)",
            "(phase 5.5)",
            "the Phase-4 gate numbers",
            "(Phase 1 acceptance: pre-init is a typed error)",
            "the Phase 4 gate numbers",
            "the task-12 constants",
            "wave task g3",
            "Plan D6a §Requirements",
            "(the GO/NO-GO input for `workflow/plans/features/x/PLAN.md`)",
            "FINDINGS #43",
            "finding #45",
            "(review finding M1)",
            "Review finding M1: the shells now",
            "the round-1 busy-spin Critical",
            "The other round-1 case: a CI rig",
            "the round-0 bug",
            "cfix-2",
            "re-review round 1",
            "review round 1",
            "device-parity-round",
            "(phase 10.B, req 4)",
            "gate-r9",
            "fixed after gate-r7 landed",
            "programmatic staged close + composed on_close (gate-r9, G53)",
            "device rounds closed G44-G54 after verification",
            "the R23 focus rule — re-created FINDINGS #43",
            "clean-signals rev `910f626` — the task-12 constants",
        ];
        for example in strip {
            assert!(
                scan_comment(example).is_some(),
                "expected STRIP {example:?}"
            );
        }
    }

    #[test]
    fn allowlist_spans_are_precise() {
        assert!(!named_rule_spans("R44-back").is_empty());
        assert!(named_rule_spans("no rule cited").is_empty());
        assert!(!external_rev_spans("clean-signals rev `910f626`").is_empty());
        assert!(external_rev_spans("no rev cited").is_empty());
    }

    #[test]
    fn comment_text_extraction_and_string_literal_heuristic() {
        assert_eq!(comment_text("let x = 5;"), None, "no `//` at all");
        assert_eq!(comment_text("// a real comment"), Some("// a real comment"));
        assert_eq!(
            comment_text("let url = \"https://example.com\";"),
            None,
            "the first `//` sits inside a string literal (odd quote count before it)"
        );
        assert_eq!(
            comment_text("let x = 5; // trailing comment"),
            Some("// trailing comment")
        );
    }

    #[test]
    fn wrapped_citations_are_caught_and_never_manufactured() {
        assert!(
            wrapped_violation(
                "// The Arc<AtomicBool> handoff the render-thread split runs on (review",
                "        // finding M1), driven here with FAKE installs",
            )
            .is_some(),
            "the parenthesized review-finding form, id on the next line"
        );
        assert!(
            wrapped_violation(
                "// The Keychain-backed arm, still unimplemented (Phase",
                "// 3) — every call returns `Unsupported`.",
            )
            .is_some(),
            "a parenthesized plan-phase whose digit wrapped"
        );
        assert_eq!(
            wrapped_violation(
                "// The dot's own oscillator: at clock phase",
                "// 0.0 it sits at the left edge, at 1.0 past the right one.",
            ),
            None,
            "a wrapped float phase is still the sanctioned animation idiom"
        );
        assert_eq!(
            wrapped_violation(
                "// A comment whose next line carries the residue on its own:",
                "// FINDINGS #43 is flagged at ITS line, not reported twice here.",
            ),
            None,
            "the following line already fails per-line — never double-reported"
        );
        assert_eq!(
            wrapped_violation(
                "// The install path refuses a second claim (review",
                "let finding = M1; // not a comment-only continuation",
            ),
            None,
            "code on the next line ends the comment run"
        );
    }

    #[test]
    fn a_wrapped_violation_is_reported_at_the_line_it_starts_on() {
        let contents = concat!(
            "fn f() {\n",
            "    // The handoff the render-thread split runs on (review\n",
            "    // finding M1), driven here with fakes.\n",
            "    g();\n",
            "}\n",
        );

        let failures = violations_in(Path::new("src/x.rs"), contents);
        assert_eq!(
            failures.len(),
            1,
            "one wrapped citation is one failure, not one per line: {failures:?}"
        );
        assert!(failures[0].starts_with("src/x.rs:2:"), "{}", failures[0]);
        assert!(failures[0].contains("finding M1)"), "{}", failures[0]);
    }

    #[test]
    fn multi_byte_comment_text_never_panics() {
        // Em dashes/arrows are common in this codebase's comments; every window/slice here must
        // land on a char boundary.
        let comment = "// the round-1 fix — an em dash — and a → arrow, plus ✓ and ζ=0.6";
        assert!(scan_comment(comment).is_some());
        assert_eq!(scan_comment("// — → ✓ ζ"), None);
    }
}
