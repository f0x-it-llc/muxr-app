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
(`connect`: 11 modules, including `qr_sheet`/`qr_affordance`/`credential_dialogs`; `sessions`:
including `manage_sheet`; `settings`: including `groups`; `terminal`: including
`panes_sheet`/`new_tab_dialog`/`link_strip`) — while `pages.rs` stays scaffold-only (route builder,
`Screen`/`State`, the `Component` impl, and action wiring). `pieces.rs` no longer exists in any
feature.

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
  Attach.
- **`terminal`** — the attached session's terminal surface: a `GlyphInk`-toned card, the fullscreen
  seam, the "panes & tabs" sheet, and the app bar's overflow menu (`frust::glyph::show_glyph_menu`,
  forwarded from `crate::shell`). A second domain port, `domain::feed::TerminalFeed` — a byte-level
  stream, deliberately separate from `TerminalRepository`'s one-shot model calls — has two
  implementations: `data::feeds::DemoFeed` (an embedded fixture replay) and
  `data::feeds::GrpcTerminalFeed` (the real `AttachTerminal` stream, a fixed 1/2/4/8/16s reconnect
  backoff, and a link-status signal the card's own link strip renders when reconnecting, disconnected
  or closed). `presentation::term` is the real grid: a `vt100` emulator held off the signal graph, a
  ~30 Hz pump coalescing every wake into one signal write, same-style run batching, cell geometry, and
  a `LAYOUT | PAINT` generation contract (the cursor blink is its one paced, paint-only exception).
  `term::input` turns platform IME snapshots into keystrokes; `term::selection`/`chip` add word-wise
  long-press selection, a re-render loupe and a sensitive-clipboard Copy chip; `presentation::keybar`
  is the IME-active-only key bar, whose caps claim (and must re-claim, on every tap) focus. Three
  things can occupy the card: a real attached session (route carries `?session=<id>` and the app holds
  a live client), the overflow menu's "Demo session" row (the fixture replay), or — a bare `/terminal`,
  and every headless suite — the seeded mock transcript `widgets::card::card_body` renders. A
  read-only credential mounts the grid in its own read-only mode and withdraws the panes sheet's
  mutating rows.
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

`connect.rs`/`sessions.rs`/`terminal.rs`/`settings.rs`/`ui_substrate.rs` exercise the four screens
and the shared `ui/` modules, each mounting its screen beside its own navigator (so it stays
visible to accessibility) and installing the app's real router over that navigator; `connect.rs`
also scripts the credential mock's recorded prompt count to assert "exactly one biometric prompt"
per flow. `term_widget.rs`/`term_input.rs`/`term_selection.rs` drive the real terminal grid, its
keystroke source and its selection/copy model directly, headless against the real `vt100`
emulator; `terminal_live.rs` mounts the whole `/terminal` screen and drives the same grid's
link-status signal through the "Demo session" cutover. See
[ARCHITECTURE.md](ARCHITECTURE.md#tests) for the shared harness and cross-cutting suites.
