//! Layering conformance scan for context/owner lookups. The bug in one
//! sentence: `GrpcAuthTokensRepository` used to resolve the app's
//! `ConnectionController` via a fresh `use_context` lookup on every RPC,
//! which panicked the instant a real call ran inside a `frust::spawn_local`
//! future (its poller installs no ambient reactive owner, so the lookup had
//! nothing to find); the fix resolves the controller once, at DI time.
//!
//! **Two scans enforce it, together.** The first
//! ([`is_sanctioned_lookup_site`]) trusts a file's *directory*:
//! `expect_connection_controller`/`expect_controller`/`use_context` may only
//! appear in a `di.rs`, `connection.rs` itself, or a `presentation`/`ui` file
//! — every other directory (`data`/`domain` included, the original bug's own
//! shape) fails. That heuristic has a blind spot: a `presentation/` file can
//! still smuggle a lookup into a spawned future's *body*, since
//! `spawn_local`'s poller installs no ambient owner regardless of directory.
//! The second scan ([`banned_lookups_in_spawn_blocks`], design/limits on its
//! own doc) closes that gap directly: a line-based `{`/`}` brace tracker flags
//! `use_context` or any `expect_*` accessor inside a tracked
//! `spawn_local(async`/`spawn(async` span, wherever it lives — its one named-
//! future blind spot is pinned separately by
//! [`the_named_future_spawned_by_the_terminal_pump_stays_lookup_free`].
//!
//! Plain `std::fs` scan, no dependency beyond std; comment-only lines are
//! stripped before matching, so a module doc naming these functions in prose
//! never trips either scan.

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

/// The three ways a reactive owner is looked up at call time — see this
/// file's own module docs for why a per-call lookup from the wrong place is
/// the owner-less spawn_local context-read panic this scan guards against.
const BANNED_NEEDLES: &[&str] = &[
    "expect_connection_controller",
    "expect_controller",
    "use_context",
];

/// True if `line` contains any [`BANNED_NEEDLES`] token.
fn has_banned_lookup(line: &str) -> bool {
    BANNED_NEEDLES.iter().any(|needle| line.contains(needle))
}

/// True if `relp` (a `/`-joined path under `src/`) is one of the three
/// sanctioned homes for a context/owner lookup:
///
/// - any `di.rs` (every feature's composition-root wiring module resolves its
///   singletons exactly once, from `MuxrApp::init`'s own ambient owner);
/// - `connection.rs` itself, which defines `expect_connection_controller`;
/// - anything under a `presentation/` directory, or under `ui/` (the app's
///   shared presentation-tier chrome, `docs/ARCHITECTURE.md`'s UI Substrate
///   section) — every call site there runs from `Component::init`/`build`,
///   both guaranteed a live owner by the framework's own rebuild contract.
///
/// A `data`/`domain` file (or anywhere else) is not on this list.
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
// Spawned-async-block scan — independent of the directory allowlist above
// (see this file's own module docs, "The directory heuristic's blind spot"
// and "Spawned-async-block scan: design and known limits").
// ---------------------------------------------------------------------------

/// Lines that open a spawned-async-block span this scan tracks through to its
/// close. Covers both a bare and a `frust::`-qualified call, and both
/// `async {}`/`async move {}`, since all four just need the substring
/// `spawn_local(async`/`spawn(async` to appear somewhere on the line.
const SPAWN_OPENERS: &[&str] = &["spawn_local(async", "spawn(async"];

/// True if `line` opens a tracked span — see [`SPAWN_OPENERS`].
fn opens_spawn_block(line: &str) -> bool {
    SPAWN_OPENERS.iter().any(|needle| line.contains(needle))
}

/// Net `{`/`}` on `line`, a plain character count — see this file's module
/// docs ("Spawned-async-block scan: design and known limits") for what this
/// does and doesn't get right.
fn brace_delta(line: &str) -> i32 {
    line.chars().filter(|&c| c == '{').count() as i32
        - line.chars().filter(|&c| c == '}').count() as i32
}

/// The spawn-block scan's own, wider matcher: `use_context` directly, plus
/// **any** `expect_*` DI-accessor call. Unlike [`BANNED_NEEDLES`] above (the
/// three names the directory-based scan enumerates because it already knows
/// every one of its sanctioned call sites), a lookup deferred into a spawned
/// future body is the same bug regardless of *which* feature's `di.rs` minted
/// the accessor — `expect_preferences_repository`, `expect_saved_servers`,
/// `expect_credential_service`, and every other feature's own `expect_*`
/// share that prefix by convention, so matching the prefix itself catches the
/// whole family without enumerating it. The one exclusion is `expect_err(` —
/// a `Result`/`Option` assertion, unrelated to context/owner lookups and
/// common in test code.
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

/// Scans `contents` line-by-line and returns the 1-based line numbers where
/// an [`is_owner_dependent_lookup`] token appears inside a tracked
/// spawned-async-block span — see [`opens_spawn_block`]/[`brace_delta`].
/// Independent of [`is_sanctioned_lookup_site`]: a spawn body is flagged
/// wherever it lives, allowlisted directory or not, since the poller that
/// drives it installs no ambient owner regardless of which file spawned it.
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

/// Guards the spawned-async-block scan's own detection logic against
/// synthetic input, independent of whatever this codebase's real source
/// currently contains — see this file's module docs for the design this
/// pins down.
#[test]
fn spawn_block_scanner_flags_only_lookups_actually_inside_the_tracked_span() {
    // A lookup inside a single-line-opened block is flagged.
    let inside =
        "fn f() {\n    spawn_local(async move {\n        expect_controller();\n    });\n}\n";
    assert_eq!(banned_lookups_in_spawn_blocks(inside), vec![3]);

    // The same token before or after the block is not flagged — only a
    // per-call lookup deferred into the future body is the bug shape.
    let outside = "fn f() {\n    let c = expect_controller();\n    spawn_local(async move {\n        c.refresh();\n    });\n}\n";
    assert!(banned_lookups_in_spawn_blocks(outside).is_empty());

    // A token on the block's own closing line is still in span.
    let on_close_line = "spawn_local(async move {\n    do_work();\n}).and(use_context::<X>());\n";
    assert_eq!(banned_lookups_in_spawn_blocks(on_close_line), vec![3]);

    // A comment-only line inside the block never trips it, same exemption
    // as the directory-based scan above.
    let commented = "spawn_local(async move {\n    // use_context here is just prose\n});\n";
    assert!(banned_lookups_in_spawn_blocks(commented).is_empty());

    // A nested spawn's body is still "inside" the outer span.
    let nested = "spawn_local(async move {\n    spawn_local(async move {\n        expect_controller();\n    });\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(nested), vec![3]);

    // A `frust::`-qualified call and a bare `spawn` (not `spawn_local`) are
    // both tracked.
    let qualified = "frust::spawn_local(async move {\n    use_context::<X>();\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(qualified), vec![2]);
    let bare_spawn = "spawn(async move {\n    use_context::<X>();\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(bare_spawn), vec![2]);

    // A call taking a named future (no literal `async` on the opening line)
    // is a known, documented limitation — not tracked.
    let named_future = "spawn_local(run_feed_watched(handle))\n    .then(|| use_context::<X>());\n";
    assert!(banned_lookups_in_spawn_blocks(named_future).is_empty());

    // Any `expect_*` DI accessor is flagged, not just the three names the
    // directory-based scan enumerates — this is the widening the class of
    // bug needs, since every feature mints its own `expect_<name>` under
    // `di.rs`.
    let other_feature_accessor =
        "spawn_local(async move {\n    expect_saved_servers().refresh().await;\n});\n";
    assert_eq!(
        banned_lookups_in_spawn_blocks(other_feature_accessor),
        vec![2]
    );

    // `expect_err(` is a `Result`/`Option` assertion, not a context/owner
    // lookup, and must not be flagged even though it shares the `expect_`
    // prefix.
    let result_assertion =
        "spawn_local(async move {\n    let e = call().await.expect_err(\"boom\");\n});\n";
    assert!(banned_lookups_in_spawn_blocks(result_assertion).is_empty());
}

/// The one spawn call in the tree whose argument is a named future rather
/// than an inline `async` block — `term/mod.rs`'s
/// `spawn_local(run_feed_watched(..))` — sits in the spawn-block scan's
/// documented blind spot (no literal `async` on the opening line to key
/// off). This pin closes that specific gap: it locates `run_feed_watched`'s
/// body by the same brace tracking the scan uses and asserts no
/// owner-dependent lookup ever appears inside it. If this test starts
/// failing, the fix is the same as everywhere else: resolve the handle
/// before the future is built and move only the resolved `Arc` in.
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
            // The signature spans lines before its `{`; only stop once the
            // body has opened and closed back to zero.
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
