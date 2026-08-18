//! Layering conformance scan for context/owner lookups. The bug: `GrpcAuthTokensRepository` used
//! to resolve `ConnectionController` via a fresh `use_context` lookup on every RPC, which
//! panicked the instant a real call ran inside a `frust::spawn_local` future (its poller installs
//! no ambient reactive owner). The fix resolves the controller once, at DI time.
//!
//! Two scans enforce it. [`is_sanctioned_lookup_site`] trusts a file's *directory*:
//! `expect_connection_controller`/`expect_controller`/`use_context` may only appear in a `di.rs`,
//! `connection.rs`, or a `presentation`/`ui` file — every other directory fails. Blind spot: a
//! `presentation/` file can still smuggle a lookup into a spawned future's *body*, since
//! `spawn_local`'s poller installs no ambient owner regardless of directory.
//! [`banned_lookups_in_spawn_blocks`] closes that gap directly: a line-based `{`/`}` brace
//! tracker flags `use_context`/any `expect_*` accessor inside a tracked
//! `spawn_local(async`/`spawn(async` span, wherever it lives — its own named-future blind spot is
//! pinned separately by [`the_named_future_spawned_by_the_terminal_pump_stays_lookup_free`].
//!
//! Plain `std::fs` scan; comment-only lines are stripped before matching, so a module doc naming
//! these functions in prose never trips either scan.

use std::fs;
use std::path::{Path, PathBuf};

/// This crate's `src/`, resolved from the manifest dir so the scan is
/// working-directory-independent.
fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// `path` relative to `src/`, forward-slashed, for stable failure messages
/// and allowlist checks independent of host path separators.
fn rel(path: &Path) -> String {
    path.strip_prefix(src_dir())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display())) {
        let path = entry.unwrap_or_else(|e| panic!("dir entry: {e}")).path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The three ways a reactive owner is looked up at call time (see module doc).
const BANNED_NEEDLES: &[&str] = &[
    "expect_connection_controller",
    "expect_controller",
    "use_context",
];

/// True if `line` contains any [`BANNED_NEEDLES`] token.
fn has_banned_lookup(line: &str) -> bool {
    BANNED_NEEDLES.iter().any(|needle| line.contains(needle))
}

/// True if `relp` is one of the three sanctioned homes for a context/owner lookup: any `di.rs`
/// (resolves singletons once from `MuxrApp::init`'s ambient owner), `connection.rs` itself
/// (defines `expect_connection_controller`), or a `presentation`/`ui` file (every call site there
/// runs from `Component::init`/`build`, guaranteed a live owner). A `data`/`domain` file (or
/// anywhere else) is not on this list.
fn is_sanctioned_lookup_site(relp: &str) -> bool {
    if relp == "connection.rs" {
        return true;
    }
    let components: Vec<&str> = relp.split('/').collect();
    if components.last() == Some(&"di.rs") {
        return true;
    }
    components
        .iter()
        .any(|c| *c == "presentation" || *c == "ui")
}

#[test]
fn a_context_owner_lookup_only_appears_at_a_sanctioned_di_time_or_presentation_site() {
    let mut failures = Vec::new();

    for path in rust_files(&src_dir()) {
        let relp = rel(&path);
        if is_sanctioned_lookup_site(&relp) {
            continue;
        }
        let contents =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));

        for (i, raw) in contents.lines().enumerate() {
            let trimmed = raw.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if has_banned_lookup(raw) {
                failures.push(format!(
                    "{relp}:{}: a context/owner lookup outside di.rs/connection.rs/presentation-or-ui \
                     — resolve the dependency once at DI time and hold it as a field instead \
                     (this is the owner-less spawn_local context-read panic this scan guards \
                     against). Line: {}",
                    i + 1,
                    raw.trim()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "layering tripwire violated ({} hit(s)) — a data/domain file (or any file outside \
         di.rs/connection.rs/presentation/ui) must never resolve `ConnectionController` (or any \
         other context value) via a per-call lookup; take it as a constructor-injected field \
         resolved once at DI time instead:\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// Guards the scanner's allowlist itself: every category it recognizes must
/// actually accept the shapes this codebase uses today, and reject the shape
/// the original bug had (a `data/repositories/*.rs` file).
#[test]
fn scanner_allowlist_recognizes_this_codebases_sanctioned_shapes() {
    assert!(is_sanctioned_lookup_site("connection.rs"));
    assert!(is_sanctioned_lookup_site("features/settings/di.rs"));
    assert!(is_sanctioned_lookup_site(
        "features/settings/presentation/pages.rs"
    ));
    assert!(is_sanctioned_lookup_site("ui/topbar.rs"));
    assert!(is_sanctioned_lookup_site("ui/backdrop.rs"));

    assert!(!is_sanctioned_lookup_site(
        "features/settings/data/repositories/grpc_auth_tokens_repository.rs"
    ));
    assert!(!is_sanctioned_lookup_site(
        "features/settings/domain/repositories.rs"
    ));
    assert!(!is_sanctioned_lookup_site("failure.rs"));
}

// ---------------------------------------------------------------------------
// Spawned-async-block scan — independent of the directory allowlist above (module doc).
// ---------------------------------------------------------------------------

/// Lines that open a spawned-async-block span this scan tracks to its close. Covers bare and
/// `frust::`-qualified calls, and `async {}`/`async move {}` — all four just need the substring
/// to appear somewhere on the line.
const SPAWN_OPENERS: &[&str] = &["spawn_local(async", "spawn(async"];

/// True if `line` opens a tracked span — see [`SPAWN_OPENERS`].
fn opens_spawn_block(line: &str) -> bool {
    SPAWN_OPENERS.iter().any(|needle| line.contains(needle))
}

/// Net `{`/`}` on `line`, a plain character count (module doc covers what this does/doesn't get
/// right).
fn brace_delta(line: &str) -> i32 {
    line.chars().filter(|&c| c == '{').count() as i32
        - line.chars().filter(|&c| c == '}').count() as i32
}

/// The spawn-block scan's wider matcher: `use_context` directly, plus **any** `expect_*`
/// DI-accessor call. Unlike [`BANNED_NEEDLES`]'s enumerated three, a lookup deferred into a
/// spawned future body is the same bug regardless of which feature's `di.rs` minted the
/// accessor, so matching the shared `expect_` prefix catches the whole family without
/// enumerating it. Exclusion: `expect_err(` is a `Result`/`Option` assertion, not a lookup.
fn is_owner_dependent_lookup(line: &str) -> bool {
    if line.contains("use_context") {
        return true;
    }
    let mut rest = line;
    while let Some(idx) = rest.find("expect_") {
        let after = &rest[idx + "expect_".len()..];
        if !after.starts_with("err(") {
            return true;
        }
        rest = &after[1..];
    }
    false
}

/// 1-based line numbers where an [`is_owner_dependent_lookup`] token appears inside a tracked
/// spawned-async-block span (see [`opens_spawn_block`]/[`brace_delta`]). Independent of
/// [`is_sanctioned_lookup_site`]: a spawn body is flagged wherever it lives, allowlisted
/// directory or not.
fn banned_lookups_in_spawn_blocks(contents: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut depth = 0i32;
    let mut in_spawn = false;

    for (i, raw) in contents.lines().enumerate() {
        let is_comment = raw.trim_start().starts_with("//");

        if !in_spawn && !is_comment && opens_spawn_block(raw) {
            in_spawn = true;
            depth = 0;
        }

        if in_spawn {
            if !is_comment && is_owner_dependent_lookup(raw) {
                hits.push(i + 1);
            }
            if !is_comment {
                depth += brace_delta(raw);
                if depth <= 0 {
                    in_spawn = false;
                }
            }
        }
    }

    hits
}

#[test]
fn a_context_owner_lookup_never_appears_inside_a_spawned_async_block() {
    let mut failures = Vec::new();

    for path in rust_files(&src_dir()) {
        let relp = rel(&path);
        let contents =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));

        for line in banned_lookups_in_spawn_blocks(&contents) {
            failures.push(format!(
                "{relp}:{line}: a context/owner lookup inside a spawned async block — the \
                 poller that later drives this future installs no ambient reactive owner, so a \
                 lookup taken from inside the block panics on the future's first real poll (the \
                 same owner-less spawn_local context-read panic the sessions poll loader had, \
                 even though this file may live in an otherwise-sanctioned directory); resolve \
                 the dependency synchronously before the `spawn`/`spawn_local` call and move \
                 only the resolved value into the future body instead."
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "layering tripwire violated ({} hit(s)) — a context/owner lookup must never be deferred \
         into a spawned async block's body, regardless of which directory the spawning file \
         lives in; resolve it synchronously at the spawn call site instead:\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// Guards the spawned-async-block scan's own detection logic against synthetic input,
/// independent of the real source tree.
#[test]
fn spawn_block_scanner_flags_only_lookups_actually_inside_the_tracked_span() {
    // Inside a single-line-opened block: flagged.
    let inside =
        "fn f() {\n    spawn_local(async move {\n        expect_controller();\n    });\n}\n";
    assert_eq!(banned_lookups_in_spawn_blocks(inside), vec![3]);

    // Same token before/after the block: not flagged, only a deferred in-body lookup is the bug.
    let outside = "fn f() {\n    let c = expect_controller();\n    spawn_local(async move {\n        c.refresh();\n    });\n}\n";
    assert!(banned_lookups_in_spawn_blocks(outside).is_empty());

    // On the block's own closing line: still in span.
    let on_close_line = "spawn_local(async move {\n    do_work();\n}).and(use_context::<X>());\n";
    assert_eq!(banned_lookups_in_spawn_blocks(on_close_line), vec![3]);

    // Comment-only line inside the block: never trips it.
    let commented = "spawn_local(async move {\n    // use_context here is just prose\n});\n";
    assert!(banned_lookups_in_spawn_blocks(commented).is_empty());

    // Nested spawn body: still "inside" the outer span.
    let nested = "spawn_local(async move {\n    spawn_local(async move {\n        expect_controller();\n    });\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(nested), vec![3]);

    // `frust::`-qualified and bare `spawn` (not `spawn_local`): both tracked.
    let qualified = "frust::spawn_local(async move {\n    use_context::<X>();\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(qualified), vec![2]);
    let bare_spawn = "spawn(async move {\n    use_context::<X>();\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(bare_spawn), vec![2]);

    // A named future (no literal `async` on the opening line): documented blind spot, not tracked.
    let named_future = "spawn_local(run_feed_watched(handle))\n    .then(|| use_context::<X>());\n";
    assert!(banned_lookups_in_spawn_blocks(named_future).is_empty());

    // Any `expect_*` accessor is flagged, not just the enumerated three — every feature mints its
    // own `expect_<name>` under `di.rs`.
    let other_feature_accessor =
        "spawn_local(async move {\n    expect_saved_servers().refresh().await;\n});\n";
    assert_eq!(
        banned_lookups_in_spawn_blocks(other_feature_accessor),
        vec![2]
    );

    // `expect_err(` is a Result/Option assertion, not a lookup — never flagged despite the prefix.
    let result_assertion =
        "spawn_local(async move {\n    let e = call().await.expect_err(\"boom\");\n});\n";
    assert!(banned_lookups_in_spawn_blocks(result_assertion).is_empty());
}

/// The one spawn call in the tree with a named-future argument rather than an inline `async`
/// block — `term/mod.rs`'s `spawn_local(run_feed_watched(..))` — sits in the spawn-block scan's
/// documented blind spot. This pin closes that gap directly: locates `run_feed_watched`'s body by
/// the same brace tracking and asserts no owner-dependent lookup appears inside it. Fix on
/// failure: resolve the handle before the future is built, move only the resolved `Arc` in.
#[test]
fn the_named_future_spawned_by_the_terminal_pump_stays_lookup_free() {
    let path = src_dir().join("features/terminal/presentation/term/mod.rs");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

    let mut depth: i32 = 0;
    let mut in_body = false;
    let mut entered = false;
    let mut offenders = Vec::new();
    for (idx, raw) in contents.lines().enumerate() {
        let line = raw.trim_start();
        if line.starts_with("//") {
            continue;
        }
        if !in_body && !entered && line.contains("pub async fn run_feed_watched") {
            in_body = true;
            entered = true;
        }
        if in_body {
            depth += brace_delta(raw);
            if is_owner_dependent_lookup(raw) {
                offenders.push(idx + 1);
            }
            // Signature spans lines before its `{`; stop only once opened and closed to zero.
            if depth == 0 && raw.contains('}') {
                in_body = false;
            }
        }
    }
    assert!(
        entered,
        "run_feed_watched not found in {} — if it moved or was renamed, \
         re-point this pin (its spawn site is the named-future shape the \
         spawn-block scan cannot track)",
        path.display()
    );
    assert!(
        offenders.is_empty(),
        "run_feed_watched's body performs an owner-dependent context lookup \
         at line(s) {offenders:?} of {} — it is polled by pump_local with no \
         ambient owner; resolve the handle before building the future",
        path.display()
    );
}
