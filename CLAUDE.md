# Muxr

Muxr is a [Frust](https://github.com/f0x-it-llc/frust)-native rewrite of the muxr client: an
app that attaches to and controls remote [zellij](https://zellij.dev) terminal-multiplexer
sessions through `muxrd`, a TLS + bearer-auth gRPC backend. Its four routed features (`connect`,
`sessions`, `terminal`, `settings`) run over the real `muxrd` gRPC layer in production (a session
guard bounces an unauthenticated push back to Connect), and a fifth, routeless `pro` slice owns the
in-app purchase and the free-tier server rule; each keeps a mock repository behind its `di.rs` seam
for headless tests, and the desktop preview degrades gracefully where a plugin has no desktop
backend (camera, biometrics, billing).

## Documentation

The project documents live in Zabin, not in this repository: project `prj_000001a07cbef4bcd3pzRYJV`
(`muxr`, shared with muxr-core), fetched by path with
`zabctl docs get --project prj_000001a07cbef4bcd3pzRYJV <path>` or the MCP `get_project_doc`;
`zabctl docs list --project prj_000001a07cbef4bcd3pzRYJV` shows the whole set.

| Topic | Zabin document |
|-------|----------------|
| Architecture (hub) — composition root, shell, navigation, tests | `docs/APP_ARCHITECTURE.md` |
| NET — gRPC transport, connection state | `docs/APP_NET_ARCHITECTURE.md` |
| UI — `src/ui/` substrate, responsive rule, resize contract, test seam | `docs/APP_UI_ARCHITECTURE.md` |
| FEATURES — feature slices, canonical screen pattern | `docs/APP_FEATURES_ARCHITECTURE.md` |
| TERMINAL — terminal slice: ports, focus, tree, rail, key bar, attach target | `docs/APP_TERMINAL_ARCHITECTURE.md` |
| Coding conventions beyond frust's `docs/CODE_STANDARDS.md` | `docs/APP_CODE_STANDARDS.md` |
| Build, run, test, flavors | `docs/APP_DEVELOPMENT.md` |
| Signing, Play Console, release | `docs/APP_RELEASE_ANDROID.md` |
| Cross-system shape, shared gates, review focus, doc budgets | `docs/ARCHITECTURE.md`, `docs/DEVELOPMENT.md`, `docs/REVIEW_FOCUS.md`, `docs/DOC_POLICY.md` |

Frust's own coding conventions apply here; see the frust checkout's `docs/CODE_STANDARDS.md`. This
repo's `docs/` keeps only design assets: the `splash.html`/`redesign.html`/`terminal.html`/`tablet.html`
mocks (`tests/splash_contract.rs` reads `splash.html`) and the `muxr-app-icons/` masters.

## Must-Know Commands

```
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

This is the app's verify gate — run it before committing. `cargo run` launches the desktop
preview. Device builds go through the `frust` CLI: `frust run --flavor develop` or
`frust run --flavor production` (Android: builds the matching Gradle flavor; iOS: builds the
`Develop`/`Production` Xcode scheme). See the Zabin `docs/APP_DEVELOPMENT.md` for the full command
set, flavor identifiers, and the test rig; `docs/APP_RELEASE_ANDROID.md` for signing and release.

## Agent Guardrails

- Every screen follows the canonical pattern in the Zabin `docs/APP_FEATURES_ARCHITECTURE.md`'s
  "Building a Screen" section (per-screen `Component`, `use_controller` lifecycle, container-free
  DI via `provide_context`/`di.rs`) — do not invent a different composition or DI shape.
- Feature code follows strict inward-only layering (`domain` framework-free, `data` implements
  `domain` repositories, `presentation` is the only layer touching `frust` widgets/signals) —
  see the Zabin `docs/APP_FEATURES_ARCHITECTURE.md` for the full rule.
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
  vector or an app-supplied `IconSource` — never a font glyph; see the Zabin
  `docs/APP_UI_ARCHITECTURE.md`'s UI Substrate section.
- Entitlement is read only through `pro::di::expect_pro_controller()`'s `ProController` — never
  granted off `request_purchase`'s return, and never mirrored into a second source of truth.
- A screen reads `responsive::Responsive::of(use_context::<WindowMetrics>())` exactly once, at the top
  of its own `Component::build`, and passes the value down — never per widget, never cached across
  builds (Zabin `docs/APP_UI_ARCHITECTURE.md`, Responsive).
