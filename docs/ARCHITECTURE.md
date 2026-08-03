# Muxr - Architecture

## Overview

Muxr is a Frust app: a stack-navigated client attaching to and controlling remote zellij sessions
through `muxrd`. All four features — `connect`, `sessions`, `terminal`, `settings` — are full
`{domain, data, presentation}` slices (mock repositories behind domain traits, clean-signals
controllers, per-screen `Component`s) over mock data; the real `muxrd` gRPC layer is not wired yet.
A persistent floating top bar and a full cold-start splash frame every screen. Every feature follows
the same inward-dependency slice, described below.

## Composition Root — `src/lib.rs`

`frust::app!(MuxrApp, setup = { frust::glyph_theme::install(); })` is the sole entry point, binding
`MuxrApp` to Android/iOS/desktop and installing Glyph as the one design system first. `init`
publishes the app-scoped `ConnectionController` (below), calls each feature's own `di::provide()`
(every feature constructs its own mock repositories the same way), and builds the app's route
`Router` alongside a second navigator, the root overlay host (`AppState::overlays`, see Navigation).
`build` mounts `shell::root_view` unchanged — its body and stable four-slot `Stack` invariant (see
Shell, below) untouched — then wraps that whole output, as its caller, in
`frust::overlay_host(&state.overlays, ..)`, so every menu/sheet/dialog renders above all chrome. Each
screen resolves `overlays` once in its own `Component::init`, exactly like `nav`.

`AppState` holds the `Router` handle plus the splash's own state; the plan is resolved lazily on the
first `build`, not `init` — at `init` time the theme isn't applied yet, so a `reduce_motion` read
there would always see the fallback baseline.

## Shell — `src/shell.rs`

`root_view` composes the whole root as one `Stack`, always the same **four fixed slots**: the
router-driven navigator (0), the persistent branded top bar (`ui::topbar`, 1) floating over it, the
app-wide toast layer (`ui::toasts`, 2), and an optional overlay tail (the cold-start splash) that
alone grows or shrinks. `Stack` reconciles positionally, and a child that changes index is torn down
and rebuilt as a new view — so the navigator, bar and toasts stay at fixed indices for the process
lifetime, and only the overlay's pod is created/destroyed, letting the splash disappear without
disturbing the live route underneath.

The bar is chrome, not layout: floating it over a full-height navigator (rather than sharing a layout
band, which used to make its appearance a page-shoving reflow) means a page's box never depends on
which route is on top. A bar-bearing page instead reserves its own top clearance via
`ui::topbar::topbar_clearance` — the status-bar inset plus the bar's height, spelled once so
`/sessions`/`/terminal`/`/settings` cannot drift apart (`/` is bar-less and wraps nothing). The bar is
never unmounted on a bar-less route (that would shift the toast layer's fixed index and tear it down);
instead it is **hidden** — zero height, no paint, no events, no semantics — and plays the identical
reveal/hide animation whether the cause is a bar-less route or the terminal screen's own fullscreen
request, a second input to the same flag published through `Nav::set_fullscreen`, never a second hide
path.

## Failure Vocabulary — `src/failure.rs`

One closed `AppFailure` enum for the whole app (`Network`, `Auth`, `Storage`, `Internal`) — small
enough not to need per-feature error types. Every use case maps its errors into this vocabulary at
the data-layer boundary; presentation code matches it exhaustively, never string-matches it. Each
variant carries its own `is_retryable` (only `Network` is) and its own user-facing message — internal
detail is never shown to the user. `ui::toasts::FailureToasts` is the one bridge from an `AppFailure`
to a visible toast, surfacing only `user_message()`.

## Navigation — `src/routes.rs`

Four flat routes, one per feature: `/`, `/sessions`, `/terminal`, `/settings`. Exactly one `Router`
exists, built once by `install_app_router` (from `MuxrApp::init`) and owned by `AppState::nav`. Since
a screen's `Component` never sees that outer state, the router is also published as a small clonable
`Nav` handle (`push`/`go`/`pop`/`resolve`/`param`) in a process-`thread_local!` (`Router` is
`Rc`-backed and so fails `provide_context`'s `Send + Sync` bound) — a screen resolves it via
`routes::app_nav()` in its own `Component::init`. `session_guard_with(Arc<ConnectionController>)` is
the router's one redirect: bounces an unauthenticated push to `/sessions`/`/terminal` back to `/`
(`/`/`/settings` never guarded), closing over the controller **by capture, not context**, so it stays
safe from any call stack; it fires only at navigation time, so a connection dropping mid-screen does
not reactively bounce it back. `ConnectionController` (`src/connection.rs`) is the app's one
mock-only connection handle, resolved via `connection::expect_connection_controller()`; it lives
beside `failure.rs`/`responsive.rs` rather than inside `connect`, since more than one feature reads
it.

`Nav` also keeps a small route-meta mirror behind the shell's one persistent top bar: `RouteMeta`/
`BarState` carry the bar's title/leading/action and the crossfade `direction`. A pop that bypasses
`Nav` (an OS back gesture on a route page) is corrected on the next depth-fall reconcile, clamping the
mirror to the deeper of its own stack and the navigator's last-published depth, discounted by any pop
already queued but not yet drained. `Nav::fullscreen()`/`set_fullscreen` publish a screen's fullscreen
request (the terminal screen's expand/collapse), and every path push carries a themed Glyph screen
transition, collapsed to a linear cross-fade under `reduce_motion`.

Every sheet, dialog and menu is a page of the root `frust::overlay_host` (`AppState::overlays`,
see Composition Root), never of this route stack, so none of the arithmetic above ever sees one. The
host routes input, and publishes accessibility nodes, for its top page only: the app underneath an
open overlay — the floating bar included — is genuinely inert rather than merely dimmed, and the
overlay's own scrim paints over the bar at the root instead of the bar mirroring a tint onto itself.
An Android back press reaches the host before the route navigator, so back dismisses the overlay
rather than popping the route beneath it, and an empty host defers the press bit for bit. A page's
pop-completion callback — a menu popping itself and chaining straight into `show_sheet` — delivers on
the pop's own frame, not the next input pass.

## Feature Slices — `src/features/<feature>/`

Four slices — `connect`, `sessions`, `terminal`, `settings` — each split into
`{domain, data, presentation}`, dependencies pointing inward only:

| Layer | May depend on | Rule |
|-------|---------------|------|
| `domain` | own `domain` + `crate::failure` + a named sibling `domain` contract + `crate::connection`'s plain-data re-exports | Framework-free: never `frust::`, never a sibling `data`/`presentation` |
| `data` | own `domain` (implements its repository traits) + external I/O crates | Framework-free like `domain`; owns its own failure-mapping into `AppFailure` |
| `presentation` | own `domain`/`data` + `frust::` | The only layer touching `frust`'s reactive/widget vocabulary |

All four are now full slices — mock repositories behind domain traits, a clean-signals controller,
and a per-screen `Component` (see Building a Screen, below) — over mock data; the real `muxrd` gRPC
layer is not wired yet (see Planned). A `domain` → sibling-`domain` edge is a deliberate, narrow
widening of the table above, used three times: `connect`'s domain re-exports
`crate::connection::TrustMode`; `terminal`'s domain re-exports `sessions`' own
`SessionsRepository`/`RenameRequest` (one owner for a session's name); and `terminal`'s domain also
re-exports `settings`' own `CursorStyle`/`PreferencesRepository` (the live pane maps a cursor shape
and reads appearance prefs through the one store `settings` writes) — no edge crosses a sibling's
`data` or `presentation`. A separate widening sits outside the layer table entirely: each feature's
`di.rs` is composition-root wiring, and `terminal`'s page resolves both
`sessions::di::expect_sessions_repository()` and `settings::di::expect_preferences_repository()`
directly rather than re-routing copies through its own.

- **`terminal`** — the attached session's terminal surface: a `GlyphInk`-toned card, a mock-mirror
  repository, the fullscreen seam, the "panes & tabs" sheet, and the app bar's overflow menu
  (`frust::glyph::show_glyph_menu`, forwarded from `crate::shell` since the bar it opens from is app
  chrome). A second domain port, `domain::feed::TerminalFeed` — a byte-level stream, deliberately
  separate from `TerminalRepository`'s one-shot model calls — has its first implementation,
  `data::feeds::DemoFeed`, replaying an embedded fixture. `presentation::term` is the real grid: a
  `vt100` emulator held off the signal graph, a ~30 Hz pump coalescing every wake into one signal
  write, same-style run batching, cell geometry, and a `LAYOUT | PAINT` generation contract (the
  cursor blink is its one paced, paint-only exception). `term::input` turns platform IME snapshots
  into keystrokes via a last-known-state diff, gated by a one-shot Ctrl/Alt latch at the single emit
  choke point; `term::selection`/`chip` add word-wise long-press selection, a re-render loupe and a
  sensitive-clipboard Copy chip; `presentation::keybar` is the IME-active-only key bar, whose caps
  claim focus and re-publish the IME surface on every tap — a pointer `Down` that claims no focus is a
  blur that drops it, so docked chrome must do the same. The screen defaults to the mock transcript;
  a "Demo session" overflow-menu row mounts this grid instead, built lazily inside `build` under the
  screen's own `Owner` so its `on_cleanup` teardown fires on unmount.
- **`settings`** — the connected server's settings screen: four groups (connection, auth tokens,
  terminal appearance, session poll) over an in-memory token repository and an in-memory preferences
  repository (persistence is Planned), a shared picker-sheet builder the last two groups' rows open,
  and the Disconnect flow — the production caller of `ConnectionController::disconnect`. Its
  `herdr version` row is a screen-local constant (see Planned).
- **`sessions`** — its `presentation::pieces` still exports an unused `picker_rows`/`picker_heading`
  pair from a picker sheet the redesign superseded (switch/create moved onto the dashed new-session
  row and the empty state); kept for a future picker sheet to inherit rather than re-derive.

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
  every app-scoped dependency (`routes::app_nav()`, the root overlay host's `overlays` handle,
  `connection::expect_connection_controller()`, a feature's own `di::expect_*`) once, in its own
  `Component::init`, and stores it in its `State` — a press handler only reads a stored handle,
  since the desktop shell dispatches events with no ambient reactive owner. A dialog/sheet a screen
  pushes is likewise a page on the *overlay host's* navigator, so its callback receives
  `&mut AppState`, never the screen's own `State` — a captured `Arc<Controller>` carries results back
  across that boundary.
- **DI: container-free composition root** (decided; `nject` evaluated and declined). App-scoped
  singletons (the connection controller, each feature's repositories) are constructed once in the
  root `Component::init` and published via `provide_context`/`provide_controller`; a screen's own
  controller comes from `use_controller`, resolving those deps with `use_context`. Each feature
  exposes a small hand-written `di.rs` wiring function — greppable, no container crate.
- **Rebuild model.** A signal write wakes one whole-app tracked rebuild, so `build` must stay cheap;
  window size/rotation/insets/theme are layout/paint-time inputs and never re-run `build`. A
  *structurally* adaptive (phone-vs-tablet) tree needs frust's still-WIP responsive primitive (see
  Responsive, below).
- **Cross-feature rule.** A feature never imports a sibling's `presentation`; app-scoped state or
  constants live in an app-level module beside `failure.rs`/`connection.rs`/`responsive.rs`, reached
  via context or a plain `crate::` import.

## UI Substrate — `src/ui/`

- **`backdrop`** — the app's full-bleed page background, shared by Connect and the splash overlay.
- **`topbar`** — the shell's persistent branded bar (see Shell, above): brand-or-back leading slot, a
  crossfading centered title, one trailing icon slot, and the bar's own reveal animation. Hosts
  `icon_button`, the app's one vector press-target primitive (a labelled `Role::Button` over a single
  icon pod) — now at three call sites (promoting it out is a deferred refactor; see Planned).
- **`sheet`** — a hand-composed modal bottom sheet (`show_sheet`/`sheet_scaffold`): the documented
  workaround for frust's missing Glyph sheet primitive. Owns `ModalStage`, the timing source its own
  scrim-fade/panel-slide shares with the Glyph dialog's dim — the sheet's scrim is its own now,
  painted at the root over the bar rather than mirrored onto it (see Navigation, above). Dismissal is
  gated through a `SheetHandle` `show_sheet` mints per instance: `close`/`close_after` pop **at most
  once**, since a duplicate close used to pop the route under the sheet instead. `show_sheet` and
  `dialogs`' helpers take only the navigator — nothing else about app state crosses that boundary.
- **`dialogs`** — Flutter-parity confirm/name-input modals (`confirm_dialog`/`name_input_dialog`)
  over `frust::glyph::glyph_dialog`, pushed as transparent navigator pages; generic over `State`.
- **`toasts`** — `FailureToasts`, the app-wide failure→toast queue, plus the `toast_layer`
  `shell::root_view` mounts permanently.
- **`splash`** — the cold-start sequence: five beats off one master clock, each independently
  mountable/testable; plays once per process launch, collapsing to a brief static hold under
  `reduce_motion`. Every timing/geometry constant is transcribed once, in `ui/splash/mod.rs::spec`,
  from `docs/splash.html`.

The terminal screen's app-bar overflow menu is `frust::glyph::show_glyph_menu`, not a `ui/` module —
it belongs to the feature that owns its entries, forwarded from `crate::shell`; the framework's
`MenuEntry` carries no icon slot, so every menu in the app today is icon-less.

**Chrome icons are vectors; font glyphs are content text only.** A bar affordance, a FAB, or a row's
trailing chevron is always a `frust::icons` vector or an app-supplied `IconSource` — never a font
glyph, whose fallback chain differs per platform and once left a settings gear invisible on iPhone. A
font glyph is only ever content *text* (a chip's `▣ 9 panes`, the splash's brand rain).

## Responsive — `src/responsive.rs`

One shared constant, `WIDE_BREAKPOINT_PX = 600.0` (Material 3's compact/medium window-size-class
boundary): every screen's `presentation` layer reads `use_context::<frust::WindowMetrics>()` once at
the top of its own `Component::build`, branches on that one constant, and never touches layout again
(`None` — no shell has published metrics yet — takes the narrow branch, same as every headless test).
Past that width each screen clamps and centers its scrolling column rather than stretching it edge to
edge, and the terminal screen additionally scales its card padding. Conservative by design: this is a
width-clamp-and-padding pass only, never a *structural* (side-by-side-pane, two-column) layout switch
— that needs frust's still-WIP responsive primitive and a design mock, neither of which exists yet.

## Tests — `tests/`

Headless integration tests drive a `RenderRoot` directly (no GPU, no window): `tests/support/` is
the shared harness (a recording paint scene, sized-frame/pump helpers, event builders, value and
rect-diff readers). `routes.rs` smoke-tests each route; `sequence.rs`/`splash.rs` exercise the splash
timeline; `navigation_motion.rs` mounts the *whole* app to exercise the shell's persistent bar, the
route-meta mirror and the page transition (a navigator's own pages are otherwise invisible to the
accessibility tree); `shell_handoff.rs` is the widget-identity regression test for the root `Stack`'s
fixed indices. `connect.rs`/`sessions.rs`/`terminal.rs`/`settings.rs`/`ui_substrate.rs` exercise the
four screens and the shared `ui/` modules, each mounting its screen beside its own navigator (so it
stays visible to accessibility) and installing the app's real router over that navigator so the
suite runs the real table and guard. `term_widget.rs`/`term_input.rs`/`term_selection.rs` drive the
real terminal grid, its keystroke source and its selection/copy model directly, headless against the
real `vt100` emulator; `terminal_live.rs` mounts the whole `/terminal` screen and exercises the same
grid through the "Demo session" cutover.

## Design References

`docs/` also holds non-code design material: `splash.html`/`redesign.html`/`terminal.html`
(interactive HTML mocks; `splash.html` is the timing/geometry source of truth above), plus
`docs/muxr-app-icons/`, the Android/iOS icon masters the build pipelines consume.

## Planned

- The `muxrd` connection/gRPC layer (TLS + bearer auth) — `connection.rs`'s controller is mock-only,
  and every feature's repositories are in-memory mocks standing in for it.
- Cutting the terminal grid over to a real `muxrd` session (replacing `DemoFeed`), a scrollback view,
  and a real session's read-only/threaded handling — the grid, its keystroke source and its
  selection are built and demo-mounted (see Feature Slices' `terminal` entry); only the live
  transport remains.
- QR-code pairing for host + token entry.
- Secure, persisted credential/preference storage — `settings`'s preferences repository is
  in-memory only.
- The login-token field's controller-held buffer is cleared and zeroized on both connect and
  disconnect today; transient copies on the render pass and the platform IME surface (suggestions,
  recents capture) still hold plaintext until platform-level masking plumbing lands.
- A `herdr` wire-version field on `ServerInfo` — the settings screen's `herdr version` row is a
  screen-local constant until then.
- Structural (side-by-side, two-column) responsive layouts, once frust's responsive primitive and a
  design mock exist.
- Promoting `icon_button` out of `ui::topbar` into its own `crate::ui` module, once it has grown a
  fourth call site.
