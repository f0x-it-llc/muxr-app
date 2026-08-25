//! Comment-density ratchet: bounds `src/`/`tests/`'s comment-line share, overall and per file,
//! so the total only shrinks over time. Tighten these caps whenever a file is deleted or its
//! comments are trimmed — never raise a cap to make room for new prose.

use std::fs;
use std::path::{Path, PathBuf};

/// This crate's own root — `tests/`/`src/` both hang directly off it.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// This test's own file, plus the sibling residue ratchet — both excluded from every check
/// below (neither is a body of application/test prose the caps are meant to bound).
fn excluded_files() -> [PathBuf; 2] {
    [
        crate_root().join("tests/comment_density.rs"),
        crate_root().join("tests/comment_residue.rs"),
    ]
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

/// Every `.rs` file across [`SCAN_ROOTS`], minus [`excluded_files`], sorted for a stable
/// failure order. Panics rather than silently scanning zero files.
fn scan_source_files() -> Vec<PathBuf> {
    let root = crate_root();
    let excluded = excluded_files();
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
    out.retain(|p| !excluded.contains(p));
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

/// True iff `line`'s first non-whitespace characters are `//` — covers `//`, `///`, and `//!`
/// alike, since the two doc-comment markers are both `//`-prefixed.
fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// One file's line/comment-line totals.
struct FileCounts {
    path: PathBuf,
    total: usize,
    comments: usize,
}

impl FileCounts {
    /// This file's comment-line share, as a percentage. `0.0` for an empty file.
    fn share_pct(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.comments as f64 / self.total as f64 * 100.0
        }
    }
}

fn count_file(path: &Path) -> FileCounts {
    let contents =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut total = 0usize;
    let mut comments = 0usize;
    for line in contents.lines() {
        total += 1;
        if is_comment_line(line) {
            comments += 1;
        }
    }
    FileCounts {
        path: path.to_path_buf(),
        total,
        comments,
    }
}

/// Running total, comment-line share of a whole scan root (`src/` or `tests/`).
#[derive(Default)]
struct RootTotals {
    total: usize,
    comments: usize,
}

impl RootTotals {
    fn share_pct(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.comments as f64 / self.total as f64 * 100.0
        }
    }
}

/// Splits `counts` into its `src/` and `tests/` running totals, by each file's relative path.
fn per_root_totals(counts: &[FileCounts]) -> (RootTotals, RootTotals) {
    let mut src = RootTotals::default();
    let mut tests = RootTotals::default();
    for c in counts {
        let bucket = if rel(&c.path).starts_with("src/") {
            &mut src
        } else {
            &mut tests
        };
        bucket.total += c.total;
        bucket.comments += c.comments;
    }
    (src, tests)
}

/// `src/`'s overall comment-line share ceiling.
const SRC_CEILING_PCT: f64 = 22.5;
/// `tests/`'s overall comment-line share ceiling.
const TESTS_CEILING_PCT: f64 = 18.0;

/// The default per-file cap applied to any file at or above [`PER_FILE_MIN_LINES`], absent an
/// [`ALLOWLIST`] entry.
const DEFAULT_PER_FILE_CAP_PCT: f64 = 25.0;
/// Files shorter than this are exempt from the per-file cap — a tiny module-index file is
/// structurally high-percentage (a two-line body under a real header reads as "mostly comment"
/// without actually carrying excess prose).
const PER_FILE_MIN_LINES: usize = 100;

/// One allowlisted file: a per-file cap tighter than [`DEFAULT_PER_FILE_CAP_PCT`] would allow,
/// with the justification for why its current share is legitimate rather than residue to trim.
struct AllowlistEntry {
    path: &'static str,
    cap_pct: f64,
    reason: &'static str,
}

/// Files whose measured comment share exceeds [`DEFAULT_PER_FILE_CAP_PCT`], each capped at its
/// own measured share (rounded up to one decimal, plus 0.5 point of headroom) rather than
/// exempted outright — the cap still ratchets down if the file shrinks or its comments do.
const ALLOWLIST: &[AllowlistEntry] = &[
    AllowlistEntry {
        path: "src/routes.rs",
        cap_pct: 38.6,
        reason: "contract-heavy nav adapter; shell/observer derivation",
    },
    AllowlistEntry {
        path: "src/features/connect/presentation/widgets/mod.rs",
        cap_pct: 36.4,
        reason: "contract-heavy: the eight-piece stagger order and the splash parity contract \
                 live in this header",
    },
    AllowlistEntry {
        path: "src/ui/splash/mod.rs",
        cap_pct: 35.1,
        reason: "contract-heavy: the module-index table is load-bearing prose, not decoration",
    },
    AllowlistEntry {
        path: "src/features/terminal/presentation/term/mod.rs",
        cap_pct: 32.7,
        reason: "contract-heavy: documents every seam and coalescing rule the submodule owns",
    },
    AllowlistEntry {
        path: "src/features/settings/domain/entities.rs",
        cap_pct: 32.3,
        reason: "contract-heavy: documents every closed choice set the settings pickers share",
    },
    AllowlistEntry {
        path: "src/features/sessions/presentation/pages.rs",
        cap_pct: 30.2,
        reason: "contract-heavy: documents the back-press/generation-cell contract beside the \
                 route it guards",
    },
    AllowlistEntry {
        path: "src/ui/splash/sequence.rs",
        cap_pct: 29.0,
        reason: "contract-heavy: the master-clock timeline table is load-bearing prose, not \
                 decoration",
    },
    AllowlistEntry {
        path: "src/features/terminal/domain/repositories.rs",
        cap_pct: 30.0,
        reason: "contract-heavy: documents which session/connection each repository method \
                 binds against",
    },
    AllowlistEntry {
        path: "src/fonts/source.rs",
        cap_pct: 29.7,
        reason: "contract-heavy: documents the two-implementation test seam and the fetch-policy \
                 bounds",
    },
    AllowlistEntry {
        path: "src/features/settings/presentation/scramble.rs",
        cap_pct: 29.3,
        reason: "contract-heavy: documents what's transcribed vs. copied from the unreachable \
                 glyph-catalog example",
    },
    AllowlistEntry {
        path: "src/features/terminal/presentation/term/geometry.rs",
        cap_pct: 29.3,
        reason: "contract-heavy: documents the measure-never-assume cell-geometry contract",
    },
    AllowlistEntry {
        path: "src/features/terminal/presentation/pages.rs",
        cap_pct: 28.1,
        reason: "contract-heavy: documents where each terminal-screen contract is enforced",
    },
    AllowlistEntry {
        path: "src/features/sessions/presentation/heartbeat.rs",
        cap_pct: 27.5,
        reason: "contract-heavy: documents the shared live-resolved cadence with the \
                 session-list poll",
    },
    AllowlistEntry {
        path: "src/features/terminal/presentation/term/style.rs",
        cap_pct: 27.4,
        reason: "contract-heavy: documents the dark-side-always palette resolution rule",
    },
    AllowlistEntry {
        path: "src/features/terminal/presentation/widgets/card.rs",
        cap_pct: 27.4,
        reason: "contract-heavy: documents the canonical-token-wins-over-mock-hex rule",
    },
    AllowlistEntry {
        path: "src/features/connect/presentation/pages.rs",
        cap_pct: 27.3,
        reason: "contract-heavy: documents the layout function the splash also assembles through",
    },
    AllowlistEntry {
        path: "src/net/error.rs",
        cap_pct: 27.1,
        reason: "contract-heavy: documents the deliberate gRPC-status collapse and its known \
                 lossiness",
    },
    AllowlistEntry {
        path: "src/features/settings/presentation/widgets/rows.rs",
        cap_pct: 25.8,
        reason: "contract-heavy: documents the never-an-emoji mark rule and the defect it fixes",
    },
    AllowlistEntry {
        path: "src/ui/backdrop.rs",
        cap_pct: 26.4,
        reason: "contract-heavy: documents why no facade widget can paint this fill instead",
    },
    AllowlistEntry {
        path: "src/features/connect/presentation/widgets/credential_dialogs.rs",
        cap_pct: 26.2,
        reason: "contract-heavy: documents the no-verification confirm gate's ordering contract",
    },
    AllowlistEntry {
        path: "src/features/terminal/presentation/term/input/utf16.rs",
        cap_pct: 25.7,
        reason: "contract-heavy for its size: short file, header on the UTF-16-at-the-seam \
                 rounding contract",
    },
];

/// The [`ALLOWLIST`] cap for `path` (relative, forward-slashed), if it has one.
fn allowlisted_cap(path: &str) -> Option<&'static AllowlistEntry> {
    ALLOWLIST.iter().find(|entry| entry.path == path)
}

#[test]
fn overall_comment_share_holds_the_ratchet() {
    let counts: Vec<FileCounts> = scan_source_files().iter().map(|p| count_file(p)).collect();
    let (src, tests) = per_root_totals(&counts);

    assert!(
        src.share_pct() <= SRC_CEILING_PCT,
        "src/ comment share is {:.2}% ({} of {} lines), over its {:.1}% ratchet ceiling — fix by \
         deleting or compressing comments across src/ (never by deleting code), then tighten \
         SRC_CEILING_PCT to match",
        src.share_pct(),
        src.comments,
        src.total,
        SRC_CEILING_PCT,
    );
    assert!(
        tests.share_pct() <= TESTS_CEILING_PCT,
        "tests/ comment share is {:.2}% ({} of {} lines), over its {:.1}% ratchet ceiling — fix \
         by deleting or compressing comments across tests/ (never by deleting code), then \
         tighten TESTS_CEILING_PCT to match",
        tests.share_pct(),
        tests.comments,
        tests.total,
        TESTS_CEILING_PCT,
    );
}

#[test]
fn per_file_comment_share_holds_its_cap() {
    let mut failures = Vec::new();
    for path in scan_source_files() {
        let counts = count_file(&path);
        if counts.total < PER_FILE_MIN_LINES {
            continue;
        }
        let location = rel(&path);
        let cap = allowlisted_cap(&location).map_or(DEFAULT_PER_FILE_CAP_PCT, |e| e.cap_pct);
        let share = counts.share_pct();
        if share > cap {
            failures.push(format!(
                "{location}: {share:.1}% comments ({} of {} lines) exceeds its {cap:.1}% cap — \
                 fix by deleting or compressing comments in this file (or, exceptionally, a \
                 justified ALLOWLIST entry here), never by deleting code",
                counts.comments, counts.total,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "per-file comment-density cap violated ({} file(s)):\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

#[cfg(test)]
mod scan_behavior {
    use super::*;

    #[test]
    fn is_comment_line_matches_all_three_markers() {
        assert!(is_comment_line("// plain"));
        assert!(is_comment_line("/// doc"));
        assert!(is_comment_line("//! inner doc"));
        assert!(is_comment_line("    // indented"));
        assert!(!is_comment_line("let x = 5; // trailing"));
        assert!(!is_comment_line("fn f() {"));
        assert!(!is_comment_line(""));
    }

    #[test]
    fn allowlisted_cap_looks_up_by_exact_relative_path() {
        assert_eq!(
            allowlisted_cap("src/routes.rs").map(|e| e.cap_pct),
            Some(38.6)
        );
        assert!(allowlisted_cap("src/lib.rs").is_none());
    }

    #[test]
    fn every_allowlist_entry_has_a_nonempty_reason() {
        for entry in ALLOWLIST {
            assert!(
                !entry.reason.trim().is_empty(),
                "allowlist entry {} carries no justification",
                entry.path
            );
        }
    }
}
