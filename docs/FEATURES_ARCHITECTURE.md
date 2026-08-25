# Muxr - FEATURES Architecture

## Overview

FEATURES covers `src/features/<feature>/` — the four `{domain, data, presentation}` slices
(`connect`, `sessions`, `terminal`, `settings`) that hold nearly all of the app's own product
code — and the canonical screen-building pattern every one of them follows.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how FEATURES relates to the rest of the app, including
the shared `ConnectionController` each slice's `data`/`presentation` layer reads (see
[NET_ARCHITECTURE.md](NET_ARCHITECTURE.md)).

## Feature Slices — `src/features/<feature>/`

Four slices — `connect`, `sessions`, `terminal`, `settings` — each split into
`{domain, data, presentation}`, dependencies pointing inward only:

| Layer | May depend on | Rule |
|-------|---------------|------|
| `domain` | own `domain` + `crate::failure` + a named sibling `domain` contract + `crate::connection`'s plain-data re-exports | Framework-free: never `frust::`, never a sibling `data`/`presentation` |
| `data` | own `domain` (implements its repository traits) + external I/O crates + (`sessions`, `terminal`, `settings` only) the one sanctioned `Arc<ConnectionController>` edge | Framework-free except `frust::spawn`/`frust::spawn_blocking`, sanctioned for background/blocking I/O off the UI thread; every other `frust::` vocabulary (signals, widgets, context, theme) stays banned, same as `domain`. Owns its own failure-mapping into `AppFailure` |
| `presentation` | own `domain`/`data` + `frust::` | The only layer touching `frust`'s reactive/widget vocabulary |

All four are full slices: a real repository over `src/net`'s gRPC client (or `crate::connection`
directly), a clean-signals controller, and a per-screen `Component` (see Building a Screen, below),
each also keeping a mock repository behind its `di.rs`'s test-only seam. A `domain` →
sibling-`domain` edge is a deliberate, narrow widening of the table above, used three times:
`connect`'s domain re-exports `crate::connection::TrustMode`; `terminal`'s domain re-exports
`sessions`' own `SessionsRepository`/`RenameRequest` (one owner for a session's name); and
`terminal`'s domain also re-exports `settings`' own `CursorStyle`/`PreferencesRepository` (the live
pane maps a cursor shape and reads appearance prefs through the one store `settings` writes) — no
edge crosses a sibling's `data` or `presentation`. Two more widenings sit outside the layer table
entirely: each feature's `di.rs` is composition-root wiring, so a page may resolve a sibling's
`di::expect_*` directly rather than re-routing a copy through its own — `terminal`'s page resolves
`sessions::di::expect_sessions_repository()`/`settings::di::expect_preferences_repository()`,
`settings`' page resolves `connect::di::expect_saved_servers()`/`expect_credential_service()`, and
`connect`'s page resolves `settings::di::expect_auth_tokens_repository()`.

Every feature's `presentation/` also carries a `widgets/` folder — one file per widget cluster
(`connect`: 10 modules, including `qr_sheet`/`qr_affordance`/`credential_dialogs`; `sessions`:
including `manage_sheet`; `settings`: including `groups`; `terminal`: including
`side_sheet_tree`/`new_tab_dialog`/`link_strip`) — while `pages.rs` stays scaffold-only (route builder,
`Screen`/`State`, the `Component` impl, and action wiring). `pieces.rs` no longer exists in any
feature.

**The `chrome.rs` catch-all pattern is gone.** `connect`, `sessions` and `settings` no longer carry
a feature-local `chrome` module at all: every box shape a widget needs — card accent, pane pills, a
`.conn-row` rule, a dashed outline — composes `frust::container`/`colored_box`/`divider` directly,
and a Glyph-catalog counterpart (`frust_glyph::radio`, the trust selector's live row) wins wherever
one exists. `settings` keeps exactly one local decorated-fill primitive, `rows::rounded_fill`, for
three gaps `frust::container` still doesn't reach: a paint-only *bleed* widening a fill past its
child's own box (`Container`/`Padding` clamp a negative inset), a zero-radius fill that paints as a
plain rect rather than a rounded one (so a paint-recording probe hunting the largest *rounded* rect
— `tests/navigation_motion.rs`'s leaving-page tracker — can't mistake this screen's full-bleed tone
for it), and a bottom hairline painted as a paint-time overlap rather than added layout height
(`widgets::pickers`' rows must stay one uniform line tall whether or not they rule). `terminal`
keeps a narrower `chrome` module too, scoped down to what has no frust counterpart at all: the
link-status strip, the animated fullscreen card stage, its own text pipeline, and a wrapping row
(`frust::Flex` has none). Everything else that module used to own — the pill/pane-item/status-row
box and the pane-title's single-line ellipsis — dissolved the same way, onto
`frust::container`/`.border_style(BorderStyle::Dashed(..))` and
`TextView::max_lines(1).overflow(TextOverflow::Ellipsis)`.

A `widgets/` file importing its own page's `Actions`/state type is a sanctioned same-layer,
same-feature back-edge, not a violation, and it happens in two forms. **Overlay-host bodies** — a
sheet or dialog widget pushed as a page of the root overlay host, outside the screen's own
component tree — import `Actions` because their callbacks run over `&mut AppState`; `sessions`'
`manage_sheet::sheet_action` is the shape. **Screen-state widgets** couple for direct state access
instead: this is the *norm* in `sessions` and `settings` (their widget files read the page's state
type or call its `pages::open_*`/`attach` shims directly), and `terminal`'s one instance is
`link_strip` — an ordinary inline card child whose Retry callback needs `&mut TerminalState`,
nothing to do with overlay hosting. `connect`'s widgets, and `terminal`'s `side_sheet_tree`, are
instead fully **page-agnostic**: the tree takes a data borrow plus an `Rc`-callback action bundle
rather than `Actions` itself, so it composes identically headless and under the sheet that mounts
it — its `Actions`-importing glue (building that bundle off the controller, and the sheet's own
footer) lives in `pages.rs`, not in `widgets/`.

- **`connect`** — the `/` route: the manual connect form, the saved-server list, and the QR-pairing
  sheet (camera scan → prefilled form → a pinned CTA the user still presses; a scan never dials by
  itself). Credentials split into two stores sharing only a `host:port` key: saved-server *metadata*
  (`shared-preferences`, key `zelli_saved_servers`) and the auth token itself, behind the device's
  biometric gate (`secure-storage`, key `zelli_cred_<host>_<port>`). Five credential flows, each its
  own controller method: manual save (mint + rotate, revoking the old token only once the new one is
  stored), one-prompt QR save (the scanned token *is* the credential, nothing minted), gated login
  with a saved credential (a row stored under `TrustMode::NoVerification` needs one more confirm gate
  before that unlock even starts — the manual form's own "Trust anyway" radio stays ungated, since
  picking it is its own consent), ungated delete (owned by Settings), and an upsert on every
  successful login (keeps the saved row's credential fields current).
- **`sessions`** — the session list: `GrpcSessionsRepository` beside the mock, addressing every
  mutation by `Session::id` (the backend-qualified routing key) rather than display name. A read-only
  credential (`ConnectionController::is_read_only`) disables Rename/Kill and session creation, never
  Attach. The first load is covered by the shared `loading` rain overlay, the last child of the
  always-present `Stack` in the scroller band (the server strip above stays live and uncovered
  throughout) — released by `SessionsController::first_load_settled` (set on either `Ok` or `Err`),
  with input falling through to the live band from the last piece's landing, not the unmount; until it
  settles, the list's trailing row is `widgets::loading_view` rather than the create row, and
  `set_loading_rain_for_tests` is the process-static seam a test binary opts out with (see
  [ARCHITECTURE.md](ARCHITECTURE.md) for the shared orchestrator).
- **`terminal`** — the attached session's terminal surface: a `GlyphInk`-toned card, the fullscreen
  seam, and the "panes & tabs" tree side sheet the app bar's one action opens directly (spaces →
  tabs → panes, zellij promoting its tabs to the top-level shape a space wears since it has none —
  a herdr session reporting zero spaces renders that same promoted two-level tree too).
  A second domain port, `domain::feed::TerminalFeed` — a byte-level
  stream, deliberately separate from `TerminalRepository`'s one-shot model calls — has two
  implementations: `data::feeds::DemoFeed` (an embedded fixture replay) and
  `data::feeds::GrpcTerminalFeed` (the real `AttachTerminal` stream, a fixed 1/2/4/8/16s reconnect
  backoff, and a link-status signal the card's own link strip renders when reconnecting, disconnected
  or closed). Switching panes, tabs, or space on a herdr backend sends `FocusPane`/`GoToTab`/
  `SwitchSpace` as unary RPCs carrying the attached stream's connection_id, never a client
  reattach: the server routes each call server-side into that connection's own relay, wire-swapping
  it onto the target pane in place, and a `GetLayout` sent with the same connection_id echoes that
  per-connection focus back — so the card's poll confirms the optimistic badge once the relay has
  applied the verb (a tick answering from before that point can revert it for ≤ one tick; the
  accepted residuals live on `commit_pane`'s doc). The tree can also browse a **non-selected**
  space's tabs without switching to it — a read-only peek (`TerminalRepository::list_tabs_in_space`),
  fail-closed against a cache the sheet has since dropped — and commit a pane there through
  `commit_pane_in_space` (switch_space → go_to_tab → focus_pane → list_spaces): the selection
  publishes optimistically ahead of the pane steps and re-lists behind them to converge. The
  tree's add-tab/add-pane rows take the same space/tab switch first, whichever level they are
  drawn under (`TerminalController::adopt_space`/`new_pane_in`) — the one way an **empty** space
  (no pane row to commit through) is reached at all. Accepted limitation: a `muxrd` predating the
  wire's `space_id` field silently answers with the focused space's tabs instead of the one asked
  for, undetectably. The feed also
  carries a controller-owned `ResumeSlot` hint — the last user-driven space/tab/pane, written only by
  a landed herdr commit or space switch, never by the layout poll — on every `AttachTerminal`,
  including the feed's own backoff reconnects, so a reconnect restores the user's view instead of
  snapping to daemon-global focus. `presentation::term` is the real grid: a `vt100` emulator held
  off the signal graph, a ~30 Hz pump coalescing every wake into one signal write, same-style run
  batching, cell geometry, and a `LAYOUT | PAINT` generation contract (the cursor blink is its one
  paced, paint-only exception).
  `term::input` turns platform IME snapshots into keystrokes; `term::selection`/`chip` add word-wise
  long-press selection, a re-render loupe and a sensitive-clipboard Copy chip; `presentation::keybar`
  is the IME-active-only key bar, whose caps claim (and must re-claim, on every tap) focus. Three
  things can occupy the card: a real attached session (route carries `?session=<id>` and the app holds
  a live client), the checked-in fixture replay (`TerminalController::enter_live`, a test-only seam
  with no menu/UI entry point), or — a bare `/terminal`, and every headless suite — the seeded mock
  transcript `widgets::card::card_body` renders. A read-only credential mounts the grid in its own
  read-only mode, withdraws the tree side sheet's mutating rows, and refuses a space row's switch
  client-side with a read-only toast, before any RPC is sent.
- **`settings`** — the connected server's settings screen: five groups (connection, auth tokens,
  saved servers, terminal appearance, session poll). `GrpcAuthTokensRepository` and `PrefsRepository`
  (the `frust-shared-preferences`-backed store) back the real run; token rows support create (with a
  read-only toggle, gated behind `is_read_only`) and revoke, and a freshly minted token's secret is
  shown once, through a dedicated reveal sheet, then gone for good. The connection group's version
  row(s) are one per `ServerInfo::backend_versions` entry; the saved servers group renders `connect`'s
  own list (`saved_server_item`, with its two delete actions). Settings owns the Disconnect flow — the
  production caller of `ConnectionController::disconnect`.

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
  [ARCHITECTURE.md](ARCHITECTURE.md#responsive)).
- **Cross-feature rule.** A feature never imports a sibling's `presentation`; app-scoped state or
  constants live in an app-level module beside `failure.rs`/`connection.rs`/`responsive.rs`, reached
  via context or a plain `crate::` import.

## Tests

`connect.rs`/`sessions.rs`/`terminal.rs`/`settings.rs`/`ui_substrate.rs` exercise the four screens and
the shared `ui/` modules, each mounting its screen beside its own navigator (so it stays visible to
accessibility) and installing the app's real router over that navigator; `connect.rs` also scripts the
credential mock's recorded prompt count to assert "exactly one biometric prompt" per flow.
`sessions_loading.rs` is the clock-driven suite proving the sessions loading-rain overlay, both
standalone and mounted under the real `/sessions` screen (see [ARCHITECTURE.md](ARCHITECTURE.md#tests)
for the shared orchestrator-level half). `term_widget.rs`/`term_input.rs`/`term_selection.rs` drive the
real terminal grid, its keystroke source and its selection/copy model directly, headless against the
real `vt100` emulator; `terminal_live.rs` mounts the whole `/terminal` screen and drives the same grid's
link-status signal through the "Demo session" cutover. See [ARCHITECTURE.md](ARCHITECTURE.md#tests) for
the shared harness and cross-cutting suites.
