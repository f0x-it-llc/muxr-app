# Muxr - Architecture

## Overview

Muxr is a Frust app: a stack-navigated client that will attach to and control remote zellij
sessions through `muxrd`. Today's tree is Phase 1 — a clean-architecture skeleton with one real
screen (Connect) and three placeholders, plus a full cold-start splash. Every feature follows the
same inward-dependency slice, described below.

## Composition Root — `src/lib.rs`

`frust::app!(MuxrApp, setup = { frust::glyph_theme::install(); })` is the app's sole entry point,
binding `MuxrApp` to the Android JNI bridge, the iOS C-ABI bridge, and the desktop preview binary
in one line; the `setup` block installs Glyph as the app's one design system before anything else
runs. `MuxrApp` (a `Component`) is stateless configuration: `init` builds the `NavigatorController`
+ route table + `Router` and resolves the start location; `build` mounts `shell::root_view` over
that router, plus — on a cold start's first build only — the splash overlay.

`AppState` (the `Component::State`) holds the `Router` handle plus the splash's own state: which
`SplashPlan` this launch resolved to, the decoded app icon (only when needed), and a `RwSignal`
the sequence flips once to signal its own completion. The plan is resolved lazily on the first
`build` rather than in `init`, because at `init` time the shell has not yet applied the theme —
reading `reduce_motion` any earlier would always see the fallback baseline.

No feature repository is constructed here yet — a future phase's real one joins this function the
same way it joins the route table, once a feature's `data` layer has one to build.

## Routing — `src/routes.rs`

Four flat routes, one per feature, no nesting: `/` (Connect), `/sessions`, `/terminal`,
`/settings`. Every route renders unconditionally today — no auth/session guard exists yet.
**Planned:** a guard bouncing an unauthenticated push to `/sessions`/`/terminal` back to `/`.

## Shell — `src/shell.rs`

`root_view` composes the app's entire root content: the router-driven navigator, with an optional
overlay stacked above it. It always returns the same `Stack` container — `[navigator]` with no
overlay, `[navigator, overlay]` with one — never a bare navigator: `AnyView` erases the concrete
view type, and swapping between `StackView` and `NavigatorView` across two builds would tear down
and rebuild the whole erased root (`AnyView::rebuild`'s `dyn_rebuild`), discarding every route's
retained widget state. A stable `Stack` root keeps the navigator at the same positional index
across the whole splash sequence, so `Stack`'s positional reconciliation rebuilds it in place; only
the overlay's own pod is torn down the frame it becomes `None`, which is what lets the cold-start
splash disappear at the end without disturbing the `/` route it hands off to.

## Failure Vocabulary — `src/failure.rs`

One closed `AppFailure` enum for the whole app (`Network`, `Auth`, `Storage`, `Internal`) — small
enough not to need per-feature error types. Every use case will map its errors into this
vocabulary at the data-layer boundary; presentation code matches it exhaustively, never
string-matches it. Each variant carries its own `is_retryable` (only `Network` is) and its own
user-facing message — the internal detail (logged via `Display`) is never shown to the user.

## Feature Slices — `src/features/<feature>/`

Four slices — `connect`, `sessions`, `terminal`, `settings` — each split into
`{domain, data, presentation}`, with dependencies pointing inward only:

| Layer | May depend on | Rule |
|-------|---------------|------|
| `domain` | this feature's own `domain` + `crate::failure` | Framework-free: never imports `frust::`, never reaches into a sibling `data`/`presentation` module |
| `data` | this feature's own `domain` (implements its repository traits) + external I/O crates | Framework-free like `domain` — no `frust::` signals/widgets; owns its own failure-mapping into `AppFailure` |
| `presentation` | this feature's own `domain`/`data` + `frust::` | The only layer touching `frust`'s reactive/widget vocabulary |

Only `connect`'s presentation is real today: a static Connect screen (eight independently
mountable pieces, arranged by `connect_layout`), no domain/data logic behind it yet. The other
three features are placeholder pages (a Glyph `app_bar` over centered text). A controller (view
model) joins a feature's presentation once its domain layer has real logic to drive.

## Building a Screen (canonical pattern)

Every screen from Phase 2 on is built this way — the shape every implementor (and, once this
repo is a public example, every frust app author) should copy. This is normative, not aspirational;
huddle (`examples/huddle`) is the framework's own reference implementation of the same shape.

- **Per-screen stateful Component** (Flutter `StatefulWidget` parity). A route builder is a plain
  `fn(...) -> AnyView<AppState>` returning `any(component(XScreen))`; `XScreen` is `impl Component`
  with its own `type State`. `component()`'s `ComponentView<C>` implements `View<Outer>` for **any**
  `Outer` state — `AnyView<AppState>` at the route seam is a boundary the adapter crosses, not a
  constraint on what a screen may itself own.
- **Lifecycle mapping.** `Component::init` + `clean_signals_frust::use_controller(factory)` gives
  `initState`/`dispose` parity: `factory` runs once on route mount (inside `init`), and the
  controller is auto-disposed via `on_cleanup` on unmount.
- **State-split rule.** Non-reactive UI state (scroll position, an open sheet, a focused field)
  lives in the Component's retained `State`; async/observed state (repository results, in-flight
  requests) lives in the hosted controller's `RwSignal`s.
- **DI: container-free composition root** (decided; `nject` evaluated and declined). App-scoped
  singletons (repositories, the future connection controller) are constructed once in the root
  `Component::init` and published via `provide_context`/`provide_controller`; a screen's own
  controller comes from `use_controller` in its own `init`, resolving those app-scoped deps with
  `use_context` inside the factory closure — huddle's exact shape, keeping construction single-site
  even though every route/`nav.push` closure reaches it. Use cases take plain constructor injection
  (`Arc<dyn Repo>`). Each feature exposes a small hand-written `di.rs` wiring function — greppable,
  compile-time-checked, no container crate; none exist yet, Phase 2's later tasks add the first.
- **Rebuild model** (framework facts). A signal write wakes one whole-app tracked rebuild, so
  `build` must stay cheap; window size/rotation/insets/theme are layout/paint-time inputs and never
  re-run `build`, and flexible layouts already adapt to them without one. A *structurally* adaptive
  (phone-vs-tablet) tree needs frust's responsive primitive, still in progress frust-side — not
  available to reach for yet.
- **Cross-feature rule** (unchanged, restated here as part of the same discipline): a feature never
  imports a sibling's `presentation`; genuinely app-scoped state lives in an app-level module beside
  `failure.rs`, reached via context, not folded into one feature's own state.

## Shared UI — `src/ui/`

- **`backdrop`** — the app's full-bleed page background (`page_backdrop`), shared by the Connect
  screen and the splash overlay; also the one place that can block pointer input while the
  overlay is on top of a still-live screen.
- **`splash`** — the cold-start sequence, below.

### Cold-Start Splash — `src/ui/splash/`

A five-beat sequence (icon spring-in → glyph burst → amber matrix rain → the Connect screen's
eight pieces staggering in → handoff), driven off one master clock; each beat is an independently
mountable, independently testable widget, sequenced by the orchestrator, and the overlay unmounts
onto a frame pixel-identical to the route it hands off to. Plays once per process launch, gated by
a process-wide flag so a later warm nav back to `/` never replays it; `Theme::motion.reduce_motion`
collapses it to a brief static-icon hold. Every timing/geometry constant is transcribed once, in
`ui/splash/mod.rs::spec`, from `docs/splash.html` — the design reference.

## Tests — `tests/`

Headless integration tests drive a `RenderRoot` directly (no GPU, no window): `tests/support/`
is the shared harness (a recording paint scene, frame/pump helpers, pointer/key event builders);
`routes.rs` smoke-tests each route renders; `sequence.rs` and `splash.rs` exercise the splash
timeline and its individual beats.

## Design References

`docs/` also holds the non-code design material: `splash.html`, `redesign.html`, and
`terminal.html` are interactive HTML mocks (the splash's is the timing/geometry source of truth
above), and `docs/muxr-app-icons/` holds the Android/iOS icon masters the build pipelines consume.

## Planned

- The `muxrd` connection/gRPC layer (TLS + bearer auth).
- Real `sessions`/`terminal`/`settings` screens, replacing today's placeholders.
- QR-code pairing for host + token entry.
- Secure credential storage.
- Terminal grid rendering.
