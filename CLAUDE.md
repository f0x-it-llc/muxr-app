# Muxr

Muxr is a [Frust](https://github.com/f0x-it-llc/frust)-native rewrite of the muxr client: an
app that attaches to and controls remote [zellij](https://zellij.dev) terminal-multiplexer
sessions through `muxrd`, a TLS + bearer-auth gRPC backend. All four features (`connect`,
`sessions`, `terminal`, `settings`) run over the real `muxrd` gRPC layer in production (a session
guard bounces an unauthenticated push back to Connect); each also keeps a mock repository behind
its `di.rs` seam for headless tests, and the desktop preview degrades gracefully where a plugin
has no desktop backend (camera, biometrics).

## Documentation

| Topic | Doc |
|-------|-----|
| Architecture (hub) — composition root, shell, navigation, UI substrate, tests | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| NET — gRPC transport, connection state | [docs/NET_ARCHITECTURE.md](docs/NET_ARCHITECTURE.md) |
| FEATURES — feature slices, canonical screen pattern | [docs/FEATURES_ARCHITECTURE.md](docs/FEATURES_ARCHITECTURE.md) |
| Build, run, test, flavors, signing | [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) |

Frust's own coding conventions apply here; see the frust checkout's `docs/CODE_STANDARDS.md`.

## Must-Know Commands

```
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

This is the app's verify gate — run it before committing. `cargo run` launches the desktop
preview. Device builds go through the `frust` CLI: `frust run --flavor develop` or
`frust run --flavor production` (Android: builds the matching Gradle flavor; iOS: builds the
`Develop`/`Production` Xcode scheme). See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the
full command set, flavor identifiers, and signing setup.

## Agent Guardrails

- Every screen follows the canonical pattern in
  [docs/FEATURES_ARCHITECTURE.md](docs/FEATURES_ARCHITECTURE.md)'s "Building a Screen" section
  (per-screen `Component`, `use_controller` lifecycle, container-free DI via
  `provide_context`/`di.rs`) — do not invent a different composition or DI shape.
- Feature code follows strict inward-only layering (`domain` framework-free, `data` implements
  `domain` repositories, `presentation` is the only layer touching `frust` widgets/signals) —
  see [docs/FEATURES_ARCHITECTURE.md](docs/FEATURES_ARCHITECTURE.md) for the full rule.
- Colors come from `ui::palette` (theme-derived: `Palette::from_theme`/`active_theme()`); spacing
  from `ui::spacing`'s named tokens — no per-feature `Palette` struct, no bare spacing literals
  for generic paddings/gaps.
- `workflow/` is never committed here (gitignored) — private planning material lives outside
  this repo.
- Android keystores and `android/app/keystores/key.properties` are never committed (gitignored);
  real keystores are stored externally, never in git.
- `Theme::glyph_baseline()` is the canonical token source for any code path that reads the
  active theme with none installed — never `Theme::neutral()` or `Theme::m3_baseline()`.
- Production code (everything outside `tests/`) imports the framework only via `frust::` and
  `frust::authoring` — never a direct `frust-core`/`frust-scene`/`frust-text` dependency.
- Never hand-edit `frust::app!`'s invocation in `src/lib.rs` or `src/main.rs` — add
  screens/state above them instead.
- Only the composition root constructs a `Router` (`routes::install_app_router`) — a screen
  reaches it through `routes::app_nav()`, never by building its own.
- Chrome icons (a bar affordance, a FAB, a row's trailing chevron) are always a `frust::icons`
  vector or an app-supplied `IconSource` — never a font glyph; see
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)'s UI Substrate section.
