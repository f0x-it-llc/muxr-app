# Muxr - Architecture

## Overview

Muxr is a Frust app: a stack-navigated client attaching to and controlling remote zellij sessions
through `muxrd`. `connect` and `sessions` are full `{domain, data, presentation}` slices (mock
repositories behind domain traits, clean-signals controllers, per-screen `Component`s);
`terminal`/`settings` remain placeholder pages. A full cold-start splash plays before the first
real screen. Every feature follows the same inward-dependency slice, described below.

## Composition Root — `src/lib.rs`

`frust::app!(MuxrApp, setup = { frust::glyph_theme::install(); })` is the sole entry point, binding
`MuxrApp` to Android/iOS/desktop and installing Glyph as the one design system first. `init`
publishes the app-scoped `ConnectionController` (below), calls each feature's own `di::provide()`
(real repository construction for `connect`/`sessions`, no-op stubs for `terminal`/`settings`), and
builds the app's one `Router`; `build` mounts `shell::root_view` over it, plus — on a cold start's
first build only — the splash overlay.

`AppState` holds the `Router` handle plus the splash's own state; the plan is resolved lazily on the
first `build`, not `init` — at `init` time the theme isn't applied yet, so a `reduce_motion` read
there would always see the fallback baseline.

## Routing — `src/routes.rs`

Four flat routes, one per feature: `/` (Connect), `/sessions`, `/terminal`, `/settings`. Exactly
one `Router` exists, built once by `install_app_router` (from `MuxrApp::init`) and owned by
`AppState::nav`. Since a screen's `Component` never sees that outer state, the router is also
published as a small clonable `Nav` handle (`push`/`go`/`resolve`) in a process-`thread_local!` —
`provide_context` requires `Send + Sync`, which the `Rc`-backed `Router` can't satisfy — reached via
`routes::app_nav()` in a screen's own `Component::init`. `install_app_router` doubles as the test
seam: a suite re-points the handle at its own navigator (why `routes` is `pub`).

`session_guard_with(Arc<ConnectionController>)` is the router's top-level redirect: bounces an
unauthenticated push to `/sessions`/`/terminal` back to `/` (`/`/`/settings` never guarded),
closing over the controller **by capture, not context** (`get_untracked`), so it stays safe from any
call stack, including a desktop press handler's ambient-owner-free one. It fires only at navigation
time, so a connection dropping mid-screen does not reactively bounce it back. `ConnectionController`
(`src/connection.rs`) is the app's one mock-only connection handle, resolved via
`connection::expect_connection_controller()`; it lives beside `failure.rs` (gating routing itself,
and read by more than one feature) rather than inside `connect`.

## Shell — `src/shell.rs`

`root_view` composes the whole root: the router-driven navigator, the app-wide toast layer
(`ui::toasts::toast_layer`) above it, and an optional overlay above both — always the same
three-slot `Stack` (`[navigator, toast layer, overlay?]`), never a bare navigator, since swapping
the erased root's concrete type across two builds (`AnyView::rebuild`) would discard every route's
retained widget state. Navigator and toast layer sit at fixed indices 0/1 for the process lifetime;
only the overlay tail grows or shrinks, so positional reconciliation rebuilds the shared indices in
place and tears down only the overlay's pod — letting the splash disappear without disturbing the
`/` route it hands off to.

## Failure Vocabulary — `src/failure.rs`

One closed `AppFailure` enum for the whole app (`Network`, `Auth`, `Storage`, `Internal`) — small
enough not to need per-feature error types. Every use case maps its errors into this vocabulary at
the data-layer boundary; presentation code matches it exhaustively, never string-matches it. Each
variant carries its own `is_retryable` (only `Network` is) and its own user-facing message — internal
detail is never shown to the user. `ui::toasts::FailureToasts` is the one bridge from an `AppFailure`
to a visible toast, surfacing only `user_message()`.

## Feature Slices — `src/features/<feature>/`

Four slices — `connect`, `sessions`, `terminal`, `settings` — each split into
`{domain, data, presentation}`, dependencies pointing inward only:

| Layer | May depend on | Rule |
|-------|---------------|------|
| `domain` | own `domain` + `crate::failure` + `crate::connection`'s plain-data re-exports | Framework-free: never `frust::`, never a sibling `data`/`presentation` |
| `data` | own `domain` (implements its repository traits) + external I/O crates | Framework-free like `domain`; owns its own failure-mapping into `AppFailure` |
| `presentation` | own `domain`/`data` + `frust::` | The only layer touching `frust`'s reactive/widget vocabulary |

`connect` and `sessions` are full slices: mock repositories behind domain traits, a clean-signals
controller, and a per-screen `Component` (see Building a Screen, below). `connect`'s domain
re-exports `crate::connection::TrustMode` rather than re-minting it — a deliberate widening of the
`domain` row above, not a violation of it. `terminal`/`settings` remain placeholder pages; a
controller joins each once its domain layer has real logic.

## Building a Screen (canonical pattern)

Every screen is built this way — the shape every implementor (and, once this repo is a public
example, every frust app author) should copy. Normative, not aspirational; huddle
(`examples/huddle`) is the framework's own reference implementation of the same shape.

- **Per-screen stateful Component** (Flutter `StatefulWidget` parity): a route builder is
  `fn(...) -> AnyView<AppState>` returning `any(component(XScreen))`; `XScreen` is `impl Component`
  with its own `type State` — `AnyView<AppState>` at the route seam is an adapter boundary only.
- **Lifecycle mapping.** `Component::init` + `clean_signals_frust::use_controller(factory)` gives
  `initState`/`dispose` parity: `factory` runs once on route mount, and the controller is
  auto-disposed via `on_cleanup` on unmount.
- **State-split rule.** Non-reactive UI state (scroll position, an open sheet, a focused field)
  lives in the retained `State`; async/observed state lives in the hosted controller's `RwSignal`s.
- **App-scoped handles and overlay callbacks respect the component boundary.** A screen resolves
  every app-scoped dependency (`routes::app_nav()`, `connection::expect_connection_controller()`, a
  feature's own `di::expect_*`) once, in its own `Component::init`, and stores it in its `State` —
  a press handler only reads a stored handle, since the desktop shell dispatches events with no
  ambient reactive owner. A dialog/sheet a screen pushes is likewise a page on the *app's*
  navigator, so its callback receives `&mut AppState`, never the screen's own `State` — a captured
  `Arc<Controller>` carries results back across that boundary.
- **DI: container-free composition root** (decided; `nject` evaluated and declined). App-scoped
  singletons (the connection controller, each feature's repository) are constructed once in the
  root `Component::init` and published via `provide_context`/`provide_controller`; a screen's own
  controller comes from `use_controller`, resolving those deps with `use_context`. Each feature
  exposes a small hand-written `di.rs` wiring function — greppable, no container crate.
- **Rebuild model.** A signal write wakes one whole-app tracked rebuild, so `build` must stay cheap;
  window size/rotation/insets/theme are layout/paint-time inputs and never re-run `build`. A
  *structurally* adaptive (phone-vs-tablet) tree needs frust's still-WIP responsive primitive.
- **Cross-feature rule.** A feature never imports a sibling's `presentation`; app-scoped state lives
  in an app-level module beside `failure.rs`/`connection.rs`, reached via context.

## Shared UI — `src/ui/`

- **`backdrop`** — the app's full-bleed page background, shared by Connect and the splash overlay.
- **`dialogs`** — Flutter-parity confirm/name-input modals (`confirm_dialog`/`name_input_dialog`)
  over `frust::glyph::glyph_dialog`, pushed as transparent navigator pages; generic over `State`.
- **`sheet`** — a hand-composed modal bottom sheet (`show_sheet`/`sheet_scaffold`): the documented
  workaround for frust's missing Glyph sheet primitive — delete once the framework grows one.
- **`toasts`** — `FailureToasts`, the app-wide failure→toast queue, plus the `toast_layer`
  `shell::root_view` mounts; `app_toasts()` is its thread-local accessor (same constraint as `Nav`).
- **`splash`** — the cold-start sequence: five beats (icon spring-in → glyph burst → amber matrix
  rain → the Connect screen's pieces staggering in → handoff) off one master clock, each
  independently mountable/testable. Plays once per process launch; `reduce_motion` collapses it to a
  brief static-icon hold. Every timing/geometry constant is transcribed once, in
  `ui/splash/mod.rs::spec`, from `docs/splash.html`.

## Tests — `tests/`

Headless integration tests drive a `RenderRoot` directly (no GPU, no window): `tests/support/` is
the shared harness (a recording paint scene, sized-frame/pump helpers, event builders, value and
rect-diff readers). `routes.rs` smoke-tests each route; `sequence.rs`/`splash.rs` exercise the
splash timeline; `connect.rs`/`sessions.rs`/`ui_substrate.rs` exercise the two real screens and the
shared `ui/` modules, each installing the app's real router over its own navigator so the suite
runs the real table and guard.

## Design References

`docs/` also holds non-code design material: `splash.html`/`redesign.html`/`terminal.html`
(interactive HTML mocks; `splash.html` is the timing/geometry source of truth above), plus
`docs/muxr-app-icons/`, the Android/iOS icon masters the build pipelines consume.

## Planned

- The `muxrd` connection/gRPC layer (TLS + bearer auth) — `connection.rs`'s controller is mock-only.
- Real `terminal`/`settings` screens, replacing today's placeholders.
- QR-code pairing for host + token entry.
- Secure credential storage.
- The login-token field is cleared from memory on both connect and disconnect
  today, but not yet masked against OS-level surfaces (IME suggestions,
  recents capture) — that needs platform plumbing this app doesn't have yet.
- Terminal grid rendering.
