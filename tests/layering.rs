//! Scans `src/` for context/owner lookups outside sanctioned directories and inside spawned async
//! blocks, where `spawn_local`'s poller installs no ambient reactive owner.
//! Comment-only lines are stripped before matching, so prose naming these functions never trips a scan.

use std::fs;
use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

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

const BANNED_NEEDLES: &[&str] = &[
    "expect_connection_controller",
    "expect_controller",
    "use_context",
];

fn has_banned_lookup(line: &str) -> bool {
    BANNED_NEEDLES.iter().any(|needle| line.contains(needle))
}

/// True if `relp` is a `di.rs`, `connection.rs`, or a `presentation`/`ui` file.
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

const SPAWN_OPENERS: &[&str] = &["spawn_local(async", "spawn(async"];

fn opens_spawn_block(line: &str) -> bool {
    SPAWN_OPENERS.iter().any(|needle| line.contains(needle))
}

fn brace_delta(line: &str) -> i32 {
    line.chars().filter(|&c| c == '{').count() as i32
        - line.chars().filter(|&c| c == '}').count() as i32
}

/// Matches `use_context` and any `expect_*` accessor; `expect_err(` is a Result assertion, not a lookup.
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

#[test]
fn spawn_block_scanner_flags_only_lookups_actually_inside_the_tracked_span() {
    let inside =
        "fn f() {\n    spawn_local(async move {\n        expect_controller();\n    });\n}\n";
    assert_eq!(banned_lookups_in_spawn_blocks(inside), vec![3]);

    let outside = "fn f() {\n    let c = expect_controller();\n    spawn_local(async move {\n        c.refresh();\n    });\n}\n";
    assert!(banned_lookups_in_spawn_blocks(outside).is_empty());

    let on_close_line = "spawn_local(async move {\n    do_work();\n}).and(use_context::<X>());\n";
    assert_eq!(banned_lookups_in_spawn_blocks(on_close_line), vec![3]);

    let commented = "spawn_local(async move {\n    // use_context here is just prose\n});\n";
    assert!(banned_lookups_in_spawn_blocks(commented).is_empty());

    let nested = "spawn_local(async move {\n    spawn_local(async move {\n        expect_controller();\n    });\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(nested), vec![3]);

    let qualified = "frust::spawn_local(async move {\n    use_context::<X>();\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(qualified), vec![2]);
    let bare_spawn = "spawn(async move {\n    use_context::<X>();\n});\n";
    assert_eq!(banned_lookups_in_spawn_blocks(bare_spawn), vec![2]);

    // A named future (no literal `async` on the opening line) is a documented blind spot.
    let named_future = "spawn_local(run_feed_watched(handle))\n    .then(|| use_context::<X>());\n";
    assert!(banned_lookups_in_spawn_blocks(named_future).is_empty());

    let other_feature_accessor =
        "spawn_local(async move {\n    expect_saved_servers().refresh().await;\n});\n";
    assert_eq!(
        banned_lookups_in_spawn_blocks(other_feature_accessor),
        vec![2]
    );

    let result_assertion =
        "spawn_local(async move {\n    let e = call().await.expect_err(\"boom\");\n});\n";
    assert!(banned_lookups_in_spawn_blocks(result_assertion).is_empty());
}

/// `term/mod.rs`'s `spawn_local(run_feed_watched(..))` takes a named future, so it is pinned separately.
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

const CONNECT_PRESENTATION: &str = "connect::presentation";

fn code_only(contents: &str) -> String {
    contents
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn bound_name(item: &str) -> String {
    let item = item.trim();
    item.rsplit_once(" as ")
        .map_or(item, |(_, alias)| alias)
        .trim()
        .to_string()
}

/// The names `mod.rs` re-exports with `pub use`, aliases resolved.
fn reexported_names(mod_rs: &str) -> Vec<String> {
    let code = code_only(mod_rs);
    let mut names = Vec::new();
    for statement in code.split(';') {
        let Some(path) = statement.trim().strip_prefix("pub use ") else {
            continue;
        };
        match path.split_once('{') {
            Some((_, list)) => names.extend(
                list.trim_end_matches('}')
                    .split(',')
                    .filter(|item| !item.trim().is_empty())
                    .map(bound_name),
            ),
            None => names.push(bound_name(path.rsplit("::").next().unwrap_or(path))),
        }
    }
    names
}

/// Each name a file reaches through `connect::presentation`; a module path or a bare module
/// import reads as its first segment, which no re-export list carries.
fn names_reached(contents: &str) -> Vec<String> {
    let code = code_only(contents);
    let mut reached = Vec::new();
    for (at, _) in code.match_indices(CONNECT_PRESENTATION) {
        let rest = &code[at + CONNECT_PRESENTATION.len()..];
        let Some(rest) = rest.strip_prefix("::") else {
            reached.push("<the module itself>".to_string());
            continue;
        };
        if let Some(list) = rest.strip_prefix('{') {
            let list = list.split('}').next().unwrap_or("");
            reached.extend(
                list.split(',')
                    .filter(|item| !item.trim().is_empty())
                    .map(|item| item.trim().split("::").next().unwrap_or("").to_string()),
            );
        } else {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            reached.push(rest[..end].to_string());
        }
    }
    reached
}

#[test]
fn settings_reaches_connects_presentation_only_through_its_re_exports() {
    let mod_rs = src_dir().join("features/connect/presentation/mod.rs");
    let exported = reexported_names(
        &fs::read_to_string(&mod_rs)
            .unwrap_or_else(|e| panic!("reading {}: {e}", mod_rs.display())),
    );
    assert!(
        !exported.is_empty(),
        "connect re-exports nothing to check against"
    );

    let mut failures = Vec::new();
    for path in rust_files(&src_dir().join("features/settings")) {
        let contents =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        for name in names_reached(&contents) {
            if !exported.contains(&name) {
                failures.push(format!("{}: reaches `{name}`", rel(&path)));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "settings imports connect's presentation only through the page-agnostic names \
         connect/presentation/mod.rs re-exports ({exported:?}):\n{}",
        failures.join("\n"),
    );
}

#[test]
fn the_re_export_scanner_reads_lists_aliases_paths_and_module_imports() {
    let mod_rs =
        "//! docs\npub mod controller;\npub use controller::{A, B as C};\npub use widgets::x::d;\n";
    assert_eq!(reexported_names(mod_rs), ["A", "C", "d"]);

    let file = "use crate::features::connect::presentation::{\n    A,\n    C,\n};\n\
                use crate::features::connect::presentation::d;\n\
                use crate::features::connect::presentation::controller::E;\n\
                use crate::features::connect::presentation;\n\
                // connect::presentation::widgets in prose\n";
    assert_eq!(
        names_reached(file),
        ["A", "C", "d", "controller", "<the module itself>"]
    );
    let nested = "use crate::features::connect::presentation::{widgets::w, A};\n";
    assert_eq!(names_reached(nested), ["widgets", "A"]);
}
