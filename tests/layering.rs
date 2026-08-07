//! Layering conformance scan — the tripwire review finding F4 asked for.
//!
//! F4's story in one sentence: `GrpcAuthTokensRepository` used to resolve the
//! app's `ConnectionController` via a fresh `use_context` lookup on every RPC,
//! which panicked the instant a real call ran inside a `frust::spawn_local`
//! future — the only poller for that future installs no ambient reactive
//! owner, so the lookup's `.expect()` had nothing to find; the fix resolves
//! the controller exactly once, at DI time, and holds it as a plain field
//! (see `GrpcAuthTokensRepository`'s own module docs).
//!
//! This scan pins that fix in place: `expect_connection_controller`,
//! `expect_controller`, and `use_context` may only appear in code that is
//! already known to run under a guaranteed-live reactive owner — a `di.rs`
//! composition-root wiring module, `connection.rs` itself (which defines
//! `expect_connection_controller`), or a `presentation`/`ui` file (every
//! screen resolves its app-scoped handles once in `Component::init`, and
//! reads context again only from `Component::build`, both of which run under
//! a live owner by construction — `docs/ARCHITECTURE.md`'s "Building a
//! Screen" section). A `data`/`domain` file reaching for any of the three —
//! the exact shape of the original bug — is not on that list and fails this
//! scan.
//!
//! Plain `std::fs` source scan, no dependency beyond std (this repo has no
//! lint-plugin tooling — see `docs/CODE_STANDARDS.md`; precedent:
//! `frust-drive/tests/print_free_cores.rs`'s own scan). Comment-only lines
//! (anything whose trimmed text starts with `//`, which covers `///`/`//!`
//! doc comments too) are stripped before matching, so a module doc that
//! *names* one of the three functions in prose — this file's own header
//! above included — never trips the scan; only an actual reference in code
//! does.

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
/// the exact bug F4 closed.
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
                     (this is the exact shape review finding F4 closed). Line: {}",
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
