# Muxr - Architecture

## Overview

Muxr is a Frust app: a stack-navigated client attaching to and controlling remote zellij sessions
through `muxrd`, a TLS + bearer-auth gRPC daemon. All four features — `connect`, `sessions`,
`terminal`, `settings` — are full `{domain, data, presentation}` slices running over `src/net`'s real
gRPC client, each also keeping a mock repository behind its `di.rs`'s test-only seam for headless
suites; the desktop preview degrades gracefully where a plugin has no desktop backend (camera,
biometrics). A persistent floating top bar and a full cold-start splash frame every screen. Every
feature follows the same inward-dependency slice, described below.

## Doc Map

| Unit | Covers | Doc |
|------|--------|-----|
| NET | `src/net/`, `src/connection.rs` — gRPC transport (TLS/auth/channel) and the app-scoped connection state machine | [NET_ARCHITECTURE.md](NET_ARCHITECTURE.md) |
| FEATURES | `src/features/*` — the four `{domain, data, presentation}` slices and the canonical screen-building pattern | [FEATURES_ARCHITECTURE.md](FEATURES_ARCHITECTURE.md) |

This hub covers composition, shell, navigation, shared UI substrate, and cross-cutting concerns
(tests, design references, planned work); each spoke owns its own subsystem in full.

## Composition Root — `src/lib.rs`

`frust::app!(MuxrApp, setup = { frust::glyph_theme::install(); })` is the sole entry point, binding
`MuxrApp` to Android/iOS/desktop and installing Glyph as the one design system first. `init`
publishes the app-scoped `ConnectionController` (see
[NET_ARCHITECTURE.md](NET_ARCHITECTURE.md)), calls each feature's own `di::provide()`
(every feature wires its own repositories the same way — real by default, mock behind a
process-static test-only seam), and builds the app's route
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
(`/`/`/settings` never guarded), closing over `ConnectionController` (see
[NET_ARCHITECTURE.md](NET_ARCHITECTURE.md)) **by capture, not context**, so it stays safe from any
call stack; it fires only at navigation time, so a
connection dropping mid-screen does not reactively bounce it back.

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

## UI Substrate — `src/ui/`

- **`palette`** — `Palette`, the app's one theme-derived colour vocabulary (`Palette::from_theme`,
  module-level `active_theme()`, `inked()` for tinting a `TextStyle`), adopted by all four
  features' `presentation::widgets` in place of each's own former per-feature `Palette` struct.
  Terminal's `GlyphInk`-sourced card tones and its ANSI palette stay terminal-local by design —
  one screen's fixed-tone card, not a role any other screen shares.
- **`spacing`** — eleven named spacing constants (`MICRO`..`HERO`) on the app's measured de-facto
  scale, adopted by every feature in place of its own local padding/gap constants; an odd one-off
  value stays feature-local rather than earning a token of its own.
- **`backdrop`** — the app's full-bleed page background, shared by Connect and the splash overlay.
- **`topbar`** — the shell's persistent branded bar (see Shell, above): brand-or-back leading slot, a
  crossfading centered title, one trailing icon slot, and the bar's own reveal animation. Hosts
  `icon_button`, the app's one vector press-target primitive (a labelled `Role::Button` over a single
  icon pod) — now at three call sites (promoting it out is a deferred refactor; see Planned).
- **`sheet`** — `show_sheet`, a thin app-facing adapter over `frust::glyph`'s `glyph_sheet`/
  `show_glyph_sheet` (the panel, its scrim, the drag handle, and every dismissal gesture belong to
  Glyph itself). This module owns only the app's own full-content-width body gutter (Glyph's
  content slot lays out under loose width) and the dismissal gate below: a `SheetHandle`
  `show_sheet` mints per instance, whose `close`/`close_then` fire **at most once** per instance,
  since a duplicate close used to pop the route under the sheet instead. `show_sheet` and
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

Headless integration tests drive a `RenderRoot` directly (no GPU, no window). `tests/support::setup`
is the one call site that opts a whole test binary onto every mock repository before `MuxrApp::init`
runs — seven seams (connection, sessions, terminal, settings' preferences and auth tokens, connect's
saved-servers and credential-service), each flipped by its own module's `use_mock_*_for_tests()`. The
same module is also the shared paint/frame/pump harness (a recording paint scene, sized-frame/pump
helpers, event builders, value and rect-diff readers). `routes.rs` smoke-tests each route;
`sequence.rs`/`splash.rs` exercise the splash timeline; `navigation_motion.rs` mounts the *whole* app
to exercise the shell's persistent bar, the route-meta mirror and the page transition (a navigator's
own pages are otherwise invisible to the accessibility tree); `shell_handoff.rs` is the
widget-identity regression test for the root `Stack`'s fixed indices. `layering.rs` is a source-scan
conformance test enforcing that owner-dependent context lookups appear only at DI/presentation-
sanctioned sites, and never inside a spawned async block's body regardless of directory.
`comment_residue.rs` is a source-scan ratchet locking the frust checkout's Comment Conventions shut
for this app's own `src/`/`tests/` — a slimmed port of frust's own conformance scan, plus a
gate/G-ledger citation detector for this repo's device-gate review shorthand. `comment_density.rs`
is the sibling comment-density ratchet: overall + per-file caps with a justified allowlist, over
the same `src/`/`tests/` roots. Per-feature and net-transport suites are covered in
[FEATURES_ARCHITECTURE.md](FEATURES_ARCHITECTURE.md#tests) and
[NET_ARCHITECTURE.md](NET_ARCHITECTURE.md#tests).

## Design References

`docs/` also holds non-code design material: `splash.html`/`redesign.html`/`terminal.html`
(interactive HTML mocks; `splash.html` is the timing/geometry source of truth above), plus
`docs/muxr-app-icons/`, the Android/iOS icon masters the build pipelines consume.

## Planned

- Scrollback-anchored selection: `term::selection` addresses only the rendered screen today and
  clears a held selection on new output rather than retarget it stale (its module docs record why).
- Pane-management RPCs (new pane, close, resize) are unwired — v2's terminal UI has no affordance for
  them yet; the dashed "new pane" row is a stand-in that reports "unavailable" rather than calling one.
- The scroll RPC is unwired too — there is no scroll gesture on the terminal grid to drive it from.
- Background pause/reattach: a backgrounded app keeps its `AttachTerminal` stream open rather than
  detaching and reattaching on resume, pending a frust app-lifecycle seam this app can hook into.
- The live terminal feed's inbound flow control (`grpc_feed.rs`) pauses reading the `AttachTerminal`
  stream once its byte or frame high-water mark trips; since every RPC shares the app's one HTTP/2
  connection (see [NET_ARCHITECTURE.md](NET_ARCHITECTURE.md)), a long pause can in principle
  backpressure that connection's flow-control window and stall the other in-flight streams
  (sessions polls, token RPCs). A dedicated
  channel for the attach stream is the designed remedy, deliberately deferred past the device gate.
- The login-token field's controller-held buffer is cleared and zeroized on both connect and
  disconnect today; transient copies on the render pass and the platform IME surface (suggestions,
  recents capture) still hold plaintext until platform-level masking plumbing lands.
- Structural (side-by-side, two-column) responsive layouts, once frust's responsive primitive and a
  design mock exist.
- Promoting `icon_button` out of `ui::topbar` into its own `crate::ui` module, once it has grown a
  fourth call site.
