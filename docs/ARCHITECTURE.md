# Muxr - Architecture

## Overview

Muxr is a Frust app: a stack-navigated client attaching to and controlling remote zellij sessions
through `muxrd`, a TLS + bearer-auth gRPC daemon. All four features — `connect`, `sessions`,
`terminal`, `settings` — are full `{domain, data, presentation}` slices running over `src/net`'s real
gRPC client, each also keeping a mock repository behind its `di.rs`'s test-only seam for headless
suites; the desktop preview degrades gracefully where a plugin has no desktop backend (camera,
biometrics). A shell-scaffold app bar frames the three connected routes, and a full cold-start splash
frames the app's first screen. Every feature follows the same inward-dependency slice, below.

## Doc Map

| Unit | Covers | Doc |
|------|--------|-----|
| NET | `src/net/`, `src/connection.rs` — gRPC transport (TLS/auth/channel) and the app-scoped connection state machine | [NET_ARCHITECTURE.md](NET_ARCHITECTURE.md) |
| FEATURES | `src/features/*` — the four `{domain, data, presentation}` slices and the canonical screen-building pattern | [FEATURES_ARCHITECTURE.md](FEATURES_ARCHITECTURE.md) |

This hub covers composition, shell, navigation, shared UI substrate, and cross-cutting concerns
(tests, design references, planned work); each spoke owns its own subsystem in full.

## Composition Root — `src/lib.rs`

`frust::app!(MuxrApp, setup = { frust::glyph_theme::install(); })` is the sole entry point, binding
`MuxrApp` to Android/iOS/desktop and installing Glyph as the one design system first. `init` publishes
the app-scoped `ConnectionController` (see [NET_ARCHITECTURE.md](NET_ARCHITECTURE.md)), calls each
feature's own `di::provide()` (every feature wires its own repositories the same way — real by
default, mock behind a process-static test-only seam), and builds the app's route `Router` alongside a
second navigator, the root overlay host (`AppState::overlays`, see Navigation). `build` mounts
`shell::root_view` unchanged — its body and stable three-slot `Stack` invariant (see Shell, below)
untouched — then wraps that whole output, as its caller, in `frust::overlay_host(&state.overlays, ..)`
(arbitration covered under Navigation). Each screen resolves `overlays` once in its own
`Component::init`, exactly like `nav`.

`AppState` holds the `Router` handle plus the splash's own state; the plan is resolved lazily on the
first `build`, not `init` — at `init` time the theme isn't applied yet, so a `reduce_motion` read
there would always see the fallback baseline.

## Shell — `src/shell.rs`

Two page builders, not one floating chrome layer. `root_view` is the app's whole root content: the
**outer** navigator (`/` and, once placed, the shell page) plus the app-wide toast layer, both fixed
`Stack` indices `Stack`'s positional reconciliation never tears down, and an optional overlay tail
(the cold-start splash) that alone grows or shrinks. The bar is no slot here — it belongs one page
down, to `shell_page`'s own scaffold, keeping it off `/` and off every in-shell navigation.

`shell_page` is the shell route's own page (see Navigation, below, for how it gets placed):
`frust::scaffold` over a **second, inner** navigator carrying the three bar-bearing routes, its
`app_bar` slot fed by `shell_bar` — a small adapter turning the inner navigator's live
`routes::RouteMeta` into a `frust_glyph::AppBarView`. Placed once and then **retained** (an in-shell
navigation issues no outer op at all), so the bar is derived fresh each rebuild from the inner
navigator's published state, never carried forward. `shell_bar` resolves to the scaffold's absent-bar
slot for a route with no `RouteMeta`, or the terminal's fullscreen request (`Nav::set_fullscreen`) —
suppressing rather than hiding, since `app_bar_opt(None)` hands the body the bar's whole band back.
The scaffold's body self-sizes *around* whatever bar is mounted (`frust_widgets::scaffold`'s
R-B4-inset contract), so a bar-bearing page reserves no clearance of its own.

## Failure Vocabulary — `src/failure.rs`

One closed `AppFailure` enum for the whole app (`Network`, `Auth`, `Storage`, `Internal`) — small
enough not to need per-feature error types. Every use case maps its errors into this vocabulary at
the data-layer boundary; presentation code matches it exhaustively, never string-matches it. Each
variant carries its own `is_retryable` (only `Network` is) and its own user-facing message — internal
detail is never shown to the user. `ui::toasts::FailureToasts` is the one bridge from an `AppFailure`
to a visible toast, surfacing only `user_message()`.

## Navigation — `src/routes.rs`

**Two navigators, one route table.** `/` (Connect) is a bar-less page of the app's **outer**
navigator (`AppState::nav`); the other three routes are children of a pathless `shell_route` bound to
a second, **inner** navigator only `crate::shell::shell_page` mounts (`/` is declared first — a
pathless shell also matches the empty segment list, and the router takes the first match). Crossing
the boundary is the outer stack's one op; navigating between `/sessions`, `/terminal` and `/settings`
lands entirely on the inner navigator and moves the outer stack not at all. Every screen reaches the
router through `Nav`, a cheap `Rc`-clonable handle (`push`/`go`/`pop`/`resolve`/`param`) resolved once
via `routes::app_nav()` in a screen's own `Component::init` — never inside a press handler — and kept
in a process `thread_local!` rather than `provide_context` (`Router`/`NavigatorController` are
`Rc`-backed and so fail its `Send + Sync` bound).

**Leaving the connected shell is a question, not a pop.** `Nav::push` applies most navigation straight
through the router; the one push it drives itself is the one that *places* the shell page, carrying
`PushOptions` a plain route builder has no seam for — the themed Glyph slide, plus
`frust::BackPolicy::DismissAnimated` and a freshly minted dismiss cell. `/sessions` is the shell's
landing route, so a back press there finds the inner navigator with nothing to pop:
`DismissAnimated` bumps that cell instead of popping the shell page (and the live connection under
it), and the Sessions screen — reading it via `sessions_back_signal()` — raises its own disconnect
confirmation; only a confirmed answer pops for real, back through the outer navigator. Every other
in-shell route keeps the default `Pop`.

**The bar is a `RouteMeta` adapter, not mirrored state.** `route_meta(path)` answers each bar-bearing
route's title/leading/action (and doubles as "does this path live inside the shell?"); `Nav::bar`
derives a fresh `BarState` every frame straight off the inner navigator's live `RouteObserver` —
nothing is cached or reconciled after the fact, so a pop `Nav` never issued (an OS back gesture)
publishes through the identical channel as one it drove. `Nav::set_fullscreen` publishes the
terminal's fullscreen request the same way (Shell, above, covers how `shell_bar` reads it), and every
path push carries a themed Glyph transition, collapsed to a linear cross-fade under `reduce_motion`.

`session_guard_with(Arc<ConnectionController>)` is the router's one redirect: the guarded set is the
shell's whole subtree, asked of `route_meta` — so `/settings` bounces while disconnected too, its only
entry point the in-shell bar gear — leaving `/` the one never-guarded route. A bounce taken with the
shell placed **unwinds** it (`Nav::unwind_shell` pops the shell page, retaining nothing authenticated
beneath Connect) rather than pushing over it; a bounce with nothing placed applies no op. It closes
over `ConnectionController` (see [NET_ARCHITECTURE.md](NET_ARCHITECTURE.md)) **by capture, not
context**, safe from any call stack, and navigation-time only — a connection dropping mid-screen does
not reactively bounce it back.

**A third navigator, outranking both.** Every sheet and dialog is a page of the root
`frust::overlay_host` (`AppState::overlays`, see Composition Root), never of either route stack: it
routes input and publishes accessibility nodes for its top page only, so the app underneath an open
overlay — the bar included — is genuinely inert rather than merely dimmed. An Android back press
reaches the host before either navigator.

## UI Substrate — `src/ui/`

- **`palette`** — `Palette`, the app's one theme-derived colour vocabulary (`Palette::from_theme`,
  `active_theme()`, `inked()` for tinting a `TextStyle`), adopted by all four features' widgets
  (terminal's `GlyphInk`-sourced tones and ANSI palette stay local by design).
- **`spacing`** — eleven named spacing constants (`MICRO`..`HERO`) on the app's measured de-facto
  scale, adopted by every feature in place of its own local padding/gap constants; an odd one-off
  value stays feature-local rather than earning a token of its own.
- **`paint`** — shared paint-primitive tokens (hairline width, pill radius, glow blur/spread, dash
  on/off step), one declaration each so no feature's hand-rolled chrome can drift from another's.
- **`backdrop`** — the app's full-bleed page background, shared by Connect and the splash overlay.
- **`topbar`** — bar *furniture*, not the bar itself: `brand_leading`/`bar_icon`, the slot contents
  `shell_bar` feeds `frust_glyph::AppBarView` (mounted in the shell scaffold's `app_bar` slot — the
  bar's own surface/inset/crossfade are all Glyph's); `icon_button`, the app's one vector press-target
  primitive (promotion out is Planned, below); and `BAR_HEIGHT`, Glyph's private compact-bar height, the
  anchor `features::terminal`'s chrome still needs as a number.
- **`sheet`** — `show_sheet`, an app-facing adapter over `frust::glyph`'s
  `glyph_sheet`/`show_glyph_sheet` (panel, scrim, drag handle, dismissal all Glyph's); owns only the
  full-content-width body gutter. `show_sheet`/`dialogs`' helpers take only the navigator — nothing else
  about app state crosses that boundary.
- **`side_sheet`** — `show_side_sheet`, the x-axis sibling adapter over `frust_glyph`'s own side sheet:
  a sticky footer slot beside the body, an `on_dismiss` fired once on any exit (scrim, close, back, or
  programmatic). Shares `sheet`'s one-shot dismissal gate, `SheetHandle` (`ui/mod.rs`):
  `close`/`close_then` fire once, so a duplicate close can't pop a route or page twice.
- **`dialogs`** — Flutter-parity confirm/name-input modals (`confirm_dialog`/`name_input_dialog`)
  over `frust::glyph::glyph_dialog`, pushed as transparent navigator pages; generic over `State`.
- **`toasts`** — `FailureToasts`, the app-wide failure→toast queue, plus the `toast_layer`
  `shell::root_view` mounts permanently.
- **`splash`** — the cold-start sequence (icon → dissolve (+burst) → assembly, unmount) off one master
  clock, each beat independently mountable/testable, once per process launch, collapsing to a static
  hold under `reduce_motion`. A second orchestrator, `loading`, plays the rain into that same assembly
  over a band still loading its own content (Sessions' first load): dense until it settles, floored at
  600ms and capped at an 8s self-release, then thin plus a staggered assembly clipped to the band,
  unmounting onto the live content. Pointer and scroll stop only until the last piece lands, then fall
  through to the identical live band; it publishes no semantics. Both orchestrators' timing/geometry
  constants transcribe once, in `ui/splash/mod.rs::spec`, from `docs/splash.html`; `loading`'s own
  hold/ceiling/offset constants are this app's tuning, not part of that transcription.

**Chrome icons are vectors; font glyphs are content text only.** A bar affordance, a FAB, or a row's
trailing chevron is always a `frust::icons` vector or an app-supplied `IconSource` — never a font
glyph, whose fallback chain differs per platform. A font glyph is only ever content *text* (a chip's
`▣ 9 panes`, the splash's brand rain).

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

Headless integration tests drive a `RenderRoot` directly (no GPU, no window). `tests/support::setup` is
the one call site that opts a whole test binary onto every mock repository before `MuxrApp::init` runs —
seven seams (connection, sessions, terminal, settings' preferences and auth tokens, connect's saved-
servers and credential-service), each flipped by its own module's `use_mock_*_for_tests()`, plus the
sessions loading-rain flag (`set_loading_rain_for_tests`), pinned off since every other suite's frame
clock sits frozen mid-rain otherwise. The same module is also the shared paint/frame/pump harness (a
recording paint scene, sized-frame/pump helpers, event builders, value and rect-diff readers).
`routes.rs` smoke-tests the route table's shape and the session guard in isolation — no reactive owner
on the stack at all, the tripwire for `session_guard_with`'s capture-not-context contract;
`guard_shell.rs` is its shell-boundary counterpart, pinning the bounce-unwind, the reconnect loop, and
dismiss-cell freshness. `sequence.rs`/`splash.rs` exercise the splash timeline; `sessions_loading.rs`
drives the `loading` orchestrator's release/hold/stagger standalone, then the mounted `/sessions` band's
hand-over to live cards (see [FEATURES_ARCHITECTURE.md](FEATURES_ARCHITECTURE.md#tests)).
`navigation_motion.rs` mounts the *whole* app (only `MuxrApp` places the scaffold, and so the bar) to
exercise the `RouteMeta` adapter, shell behavior (bar-less Connect, a titled route, direction on back,
terminal fullscreen suppression, the settings → sessions → connect round trip) and overlay-host
arbitration (R44-back, a modal dimming the bar) — read off the accessibility tree, since a navigator's
own pages are otherwise invisible to it; `shell_handoff.rs` is the widget-identity regression test for
the root `Stack`'s fixed indices. `layering.rs` is a source-scan conformance test enforcing that owner-
dependent context lookups appear only at DI/presentation-sanctioned sites, and never inside a spawned
async block's body regardless of directory. `comment_residue.rs` is a source-scan ratchet locking the
frust checkout's Comment Conventions shut for this app's own `src/`/`tests/` — a slimmed port of frust's
own conformance scan, plus a gate/G-ledger citation detector for this repo's device-gate review
shorthand. `comment_density.rs` is the sibling comment-density ratchet: overall + per-file caps with a
justified allowlist, over the same `src/`/`tests/` roots. Per-feature and net-transport suites are
covered in [FEATURES_ARCHITECTURE.md](FEATURES_ARCHITECTURE.md#tests) and
[NET_ARCHITECTURE.md](NET_ARCHITECTURE.md#tests).

## Design References

`docs/` also holds design material: the `splash.html`/`redesign.html`/`terminal.html` interactive
mocks, plus `docs/muxr-app-icons/`, the Android/iOS icon masters the build pipelines consume.

## Planned

- Scrollback-anchored selection: `term::selection` addresses only the rendered screen and clears a
  held selection on new output rather than retargeting it stale (its module docs record why).
- Space/tab/pane create and rename RPCs are wired (add rows, press-and-hold; new-pane creation is
  tap-only, no floating-vs-tiled toggle); close, resize, and scroll are not — v2's terminal UI has
  no affordance for any of them, including a scroll gesture to drive the RPC from.
- Background pause/reattach: a backgrounded app keeps its `AttachTerminal` stream open rather than
  detaching/reattaching on resume, pending a frust app-lifecycle seam to hook into.
- The live terminal feed's inbound flow control can backpressure the app's one shared HTTP/2
  connection and stall other in-flight RPCs (see [NET_ARCHITECTURE.md](NET_ARCHITECTURE.md)); a
  dedicated attach-stream channel is the designed remedy, deferred past the device gate.
- The login-token field's buffer is zeroized on connect/disconnect; transient copies on the render
  pass and platform IME surface (suggestions, recents) still hold plaintext until masking lands.
- Structural (side-by-side, two-column) layouts, pending frust's WIP responsive primitive and a design mock.
- Promoting `icon_button` out of `ui::topbar` into its own module — the fourth-call-site trigger fired.
