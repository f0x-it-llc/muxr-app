# Muxr - FEATURES Architecture

## Overview

FEATURES covers `src/features/<feature>/` — the five `{domain, data, presentation}` slices (`connect`,
`sessions`, `terminal`, `settings`, and the routeless `pro`) holding nearly all of this app's product
code — and the canonical screen-building pattern every one of them follows. See
[ARCHITECTURE.md](ARCHITECTURE.md) for how FEATURES relates to the rest of the app, and
[NET_ARCHITECTURE.md](NET_ARCHITECTURE.md) for the shared `ConnectionController` these slices read.

## Feature Slices — `src/features/<feature>/`

Every slice splits into `{domain, data, presentation}`, dependencies pointing inward only:

| Layer | May depend on | Rule |
|-------|---------------|------|
| `domain` | own `domain` + `crate::failure` + a named sibling `domain` contract + `crate::connection`'s plain-data re-exports | Framework-free: never `frust::`, never a sibling `data`/`presentation` |
| `data` | own `domain` (implements its repository traits) + external I/O crates + (`sessions`, `terminal`, `settings` only) the one sanctioned `Arc<ConnectionController>` edge | Framework-free except `frust::spawn`/`frust::spawn_blocking`, sanctioned for background/blocking I/O off the UI thread; every other `frust::` vocabulary (signals, widgets, context, theme) stays banned, same as `domain`. Owns its own failure-mapping into `AppFailure` |
| `presentation` | own `domain`/`data` + `frust::` | The only layer touching `frust`'s reactive/widget vocabulary |

All five are full slices: a real repository (over `src/net`'s gRPC client, `crate::connection`, or the
platform store), a clean-signals controller, and — for the four routed ones — a per-screen `Component`
(see Building a Screen), each also keeping a mock behind its `di.rs`'s test-only seam. A `domain` →
sibling-`domain` edge is a deliberate, narrow widening of the table above, used four times:
`connect`'s domain re-exports `crate::connection::TrustMode`; `terminal`'s re-exports `sessions`'
`SessionsRepository`/`RenameRequest` (one owner for a session's name) and `settings`'
`CursorStyle`/`PreferencesRepository` (the live pane maps a cursor shape and reads appearance prefs
through the one store `settings` writes); and `pro` answers its free-tier verdict in `connect`'s own
`SavedServer` rows — the one edge a `data` layer takes too, for that same module's `epoch_millis`. No
such edge crosses a sibling's `data` or `presentation`. One more widening sits outside the table:
`di.rs` is composition-root wiring, so a page may resolve a sibling's `di::expect_*` directly rather
than re-route a copy through its own — `terminal` →
`sessions::di::expect_sessions_repository()`/`settings::di::expect_preferences_repository()`,
`settings` → `connect::di::expect_saved_servers()`/`expect_credential_service()`, `connect` →
`settings::di::expect_auth_tokens_repository()`, and both of the last two →
`pro::di::expect_pro_controller()` for the entitlement their gates read.

Every feature's `presentation/` also carries a `widgets/` folder — one file per widget cluster
(`connect`: 11 modules, including `qr_sheet`/`qr_affordance`/`credential_dialogs`/`pro_gate`;
`sessions`: `manage_sheet` among them; `settings`: `groups`; `terminal`:
`side_sheet_tree`/`new_tab_dialog`/`link_strip`; `pro`: `upgrade_sheet` alone) — while `pages.rs`
stays scaffold-only (route builder, `Screen`/`State`, the `Component` impl, action wiring) and no
feature carries a `pieces.rs`.

**There is no `chrome.rs` catch-all.** `connect`, `sessions` and `settings` carry no feature-local
`chrome` module at all: card accent, pane pills, a `.conn-row` rule, a dashed outline and a one-line
ellipsis compose `frust::container`/`colored_box`/`divider`, `.border_style(BorderStyle::Dashed(..))`
and `TextView::max_lines(1).overflow(TextOverflow::Ellipsis)` directly, preferring a Glyph-catalog
counterpart (`frust_glyph::radio`) wherever one exists. Two exceptions: `settings`'
`rows::rounded_fill`, for three gaps `frust::container` doesn't reach (a negative-inset *bleed*
`Container`/`Padding` would clamp, a zero-radius fill `tests/navigation_motion.rs`'s
largest-*rounded*-rect tracker must not read as a page, and a paint-time hairline keeping
`widgets::pickers`' rows one line tall), and `terminal`'s own narrower `chrome` (link-status strip,
animated fullscreen card stage, text pipeline, and a wrapping row `frust::Flex` has none of).

A `widgets/` file importing its own page's `Actions`/state type is a sanctioned same-layer back-edge,
in two forms. **Overlay-host bodies** — pushed as a page of the root overlay host, outside the
screen's own component tree — import `Actions` because their callbacks run over `&mut AppState`;
`sessions`' `manage_sheet::sheet_action` is the shape. **Screen-state widgets** couple for direct
state access instead: the *norm* in `sessions` and `settings` (whose widget files read the page's
state type or call its `pages::open_*`/`attach` shims), and `terminal`'s `link_strip`, an inline card
child whose Retry callback needs `&mut TerminalState`. `connect`'s widgets, `terminal`'s
`side_sheet_tree` and `pro`'s `upgrade_sheet` are instead fully **page-agnostic**: each takes a data
borrow or controller handle plus an `Rc`-callback bundle rather than `Actions`, so it composes
identically headless and under the sheet that mounts it — the tree's `Actions`-importing glue lives in
`pages.rs`, and the paywall's callbacks take no state at all, since `ui::SheetHandle::close_then` runs
its follow-on from Glyph's own `on_close`, where no `&mut AppState` exists.

- **`connect`** — the `/` route: the manual connect form, the saved-server list, and the QR-pairing sheet
  (camera scan → prefilled form → a pinned CTA; a scan never dials by itself). Credentials split into two
  stores sharing a `host:port` key: saved-server *metadata* (`shared-preferences`, `zelli_saved_servers`)
  and the auth token, behind the device's biometric gate (`secure-storage`, `zelli_cred_<host>_<port>`).
  Five credential flows, each a controller method: manual save (mint + rotate, revoking the old token only
  once the new one is stored), one-prompt QR save (the scanned token *is* the credential), gated login
  with a saved credential (a row stored under `TrustMode::NoVerification` needs one more confirm gate
  first; the manual form's "Trust anyway" radio stays ungated, picking it is consent), ungated delete
  (owned by Settings), and an upsert on every successful login. **Every dial takes `pro`'s free-tier
  verdict before it reaches the wire**, raising the paywall through a `GatePrompt` the screen lends per
  press instead of dialing: `ConnectController::connect`/`connect_from_pairing` take it inside the entry
  point (busy → target → verdict → dial), while the saved-row tap is gated one level up in
  `widgets::pro_gate::gated_saved_row_tap`, so the paywall precedes that row's own no-verification confirm
  and `recover_and_connect` stays gate-free by contract. The block carries a `ConnectRetry` naming the
  entry point, so unlocking Pro — or `forget_saved_servers_then` dropping the rows the free tier can't
  keep — re-enters the connect the user asked for.
- **`sessions`** — the session list: `GrpcSessionsRepository` beside the mock, addressing mutations by
  `Session::id` (the backend-qualified routing key), not display name. A read-only credential
  (`ConnectionController::is_read_only`) disables Rename/Kill and creation, never Attach. The first load
  is covered by the shared `loading` rain overlay — the scroller band's always-present `Stack`'s last
  child, under a live server strip, released by `SessionsController::first_load_settled` on `Ok` or `Err`;
  input stays on the overlay past the last piece's landing while the two session sets still differ
  (membership/order, not telemetry; [ARCHITECTURE.md](ARCHITECTURE.md) owns freeze/unmount). Until settled
  the trailing row is `widgets::loading_view`, not the create row; `set_loading_rain_for_tests` is the
  process-static seam a test binary opts out with.
- **`terminal`** — the attached session's surface: a `GlyphInk`-toned card, the fullscreen seam, and the
  "panes & tabs" tree side sheet the app bar's one action opens (spaces → tabs → panes; zellij and a
  zero-space herdr session promote tabs to the top). `domain::feed::TerminalFeed` is a second domain
  port — a byte-level stream, separate from `TerminalRepository`'s one-shot model calls — implemented
  by `data::feeds::DemoFeed` (an embedded fixture replay) and
  `data::feeds::GrpcTerminalFeed` (the real `AttachTerminal` stream, a fixed 1/2/4/8/16s reconnect
  backoff, and the link-status signal the card's link strip renders). **Focus moves server-side, never
  by reattaching**: the herdr verbs (`FocusPane`/`GoToTab`/`SwitchSpace`) and the non-selected-space
  peek/commit pair (`TerminalRepository::list_tabs_in_space` fail-closed, then `commit_pane_in_space`
  or `TerminalController::adopt_space`/`new_pane_in`, the only way an **empty** space is reached) all
  go as unary RPCs on the attached stream's connection_id, and a `GetLayout` on that id echoes the
  focus back to confirm the card's optimistic badge (`commit_pane`'s doc holds the ≤ one-tick
  residuals). Accepted limitation: a `muxrd` predating the wire's `space_id` field answers for the
  focused space instead, undetectably. Every `AttachTerminal`, backoff reconnects included, carries a
  controller-owned `ResumeSlot` — the last user-driven space/tab/pane, never the layout poll's — so a
  reconnect restores that view. `presentation::term` is the real grid: a `vt100` emulator held off the
  signal graph, a ~30 Hz pump coalescing wakes into one signal write, same-style run batching, cell
  geometry, and a `LAYOUT | PAINT` generation contract (the paced cursor blink its one paint-only
  exception); `term::input` turns IME snapshots into keystrokes, `term::selection`/`chip` add
  word-wise long-press selection, a re-render loupe and a sensitive-clipboard Copy chip, and
  `presentation::keybar` is the IME-active-only key bar whose caps re-claim focus on every tap. Three
  things can occupy the card: a live attached session (`?session=<id>`), the checked-in fixture replay
  (`TerminalController::enter_live`, a test-only seam, no menu entry), or — a bare `/terminal`, and
  every headless suite — the mock transcript `widgets::card::card_body` renders. A read-only
  credential mounts the grid read-only, withdraws the tree's mutating rows, and refuses a space
  switch client-side.
- **`settings`** — the connected server's settings screen: six groups (connection, auth tokens, saved
  servers, `muxr pro`, terminal appearance, session poll). `GrpcAuthTokensRepository` and
  `PrefsRepository` (the `frust-shared-preferences`-backed store) back the real run; token rows
  support create (with a read-only toggle, gated behind `is_read_only`) and revoke, and a freshly
  minted secret shows once, in a reveal sheet, then is gone. The connection group's version row(s) are
  one per `ServerInfo::backend_versions` entry; the saved servers group renders `connect`'s own list
  (`saved_server_item`, with its two delete actions); the `muxr pro` group is a plan row naming this
  install's entitlement plus, short of Pro, the Upgrade/Restore actions — over a `ProController`
  resolved once in `Component::init` and refreshed on every mount. Settings owns the Disconnect flow,
  the production caller of `ConnectionController::disconnect`.
- **`pro`** — muxr's one in-app purchase and the free-tier rule it lifts; the only routeless slice.
  `Entitlement` (`Free`/`Pro`/`Pending` — unsettled, unlocking nothing), `ProFeature`, and
  `server_limit_verdict` — a free install keeps only the server it's connecting to; blocked, it names the
  rest to forget. `EntitlementRepository` has three implementations: `IapEntitlementRepository`
  (`frust-iap` over an `EntitlementCache` of two `frust-shared-preferences` keys, Free when absent, so a
  cold or offline start gates on it), `MockEntitlementRepository`, and the degraded-launch
  `UnavailableEntitlementRepository` (sells nothing, grants nothing). `di::provide()` publishes one (the
  mock under the test seam or a `mock-iap` build, the degraded store when the real one's prefs cache
  won't open, else the real store) plus the app-scoped `ProController` over it — entitlement seeded from
  the cache, `busy` a single-flight guard both paywalls share; only the test seam also publishes the mock
  handle. A restore answers a `RestoreResult` (`Found(entitlement)` or `Unavailable`, a store never
  asked), read back as a `RestoreOutcome`
  (`Restored`/`Pending`/`NothingToRestore`/`Unavailable`/`Failed`/`Busy`); callers toast only the store's
  own answers; a `Failed` reaches toasts via the controller's failure sink, a purchase's failure is the
  sheet's alone (its run stays off the sink), background refreshes and the offer lookup fail quietly —
  nothing is announced twice. **The purchase contract is `frust-iap`'s**: Pro is granted only off a
  store-reported `Purchased` row, never `request_purchase`'s return — an error proving the flow never
  started (a dropped connection is reopened and retried once; a host refusal answers from its code) fails
  fast, and only the documented ack-timeout still waits on the listener — acknowledged once the grant is
  durable (Play refunds unacknowledged ones); re-queried at launch, each Settings mount and after
  purchase/restore, *overwriting* the cache so a refund reaches Free; failures log a store code, never a
  token. Accepted limits: a client-side grant, **no server-side receipt verification** (no backend
  exists); a failed offer lookup reads "unavailable" like an unsellable store, no retry affordance.

## Building a Screen (canonical pattern)

Every screen is built this way — normative, not aspirational; huddle (`examples/huddle`) is the reference.

- **Per-screen stateful Component** (Flutter `StatefulWidget` parity): a route builder is
  `fn(...) -> AnyView<AppState>` returning `any(component(XScreen))`; `XScreen` is `impl Component`
  with its own `type State` — `AnyView<AppState>` at the route seam is an adapter boundary only.
- **Lifecycle mapping.** `Component::init` + `clean_signals_frust::use_controller(factory)` gives
  `initState`/`dispose` parity: `factory` runs once on route mount, the controller auto-disposed via
  `on_cleanup` on unmount.
- **State-split rule.** Non-reactive UI state (scroll position, an open sheet, a focused field) lives in
  the retained `State`; async/observed state lives in the hosted controller's `RwSignal`s.
- **App-scoped handles and overlay callbacks respect the component boundary.** A screen resolves every
  app-scoped dependency (`routes::app_nav()`, the root overlay host's `overlays` handle,
  `connection::expect_connection_controller()`, a feature's own `di::expect_*`) once, in its own
  `Component::init`, and stores it in its `State` — a press handler only reads a stored handle, since
  the desktop shell dispatches events with no ambient reactive owner. A dialog/sheet a screen pushes
  is a page on the *overlay host's* navigator, so its callback receives `&mut AppState`, never the
  screen's `State` — a captured `Arc<Controller>` carries results back across it.
- **DI: container-free composition root** (decided; `nject` evaluated and declined). App-scoped
  singletons (the connection controller, each feature's repositories) are constructed once in the root
  `Component::init` and published via `provide_context`/`provide_controller`; a screen's own
  controller comes from `use_controller`, resolving those deps with `use_context`. Each feature
  exposes a small hand-written `di.rs` wiring function — greppable, no container crate.
- **Rebuild model.** A signal write wakes one whole-app tracked rebuild, so `build` must stay cheap;
  window size/rotation/insets/theme are layout/paint-time inputs and never re-run `build`. A
  *structurally* adaptive (phone-vs-tablet) tree needs frust's still-WIP responsive primitive
  ([ARCHITECTURE.md](ARCHITECTURE.md#responsive)).
- **Cross-feature rule.** A feature imports no sibling's `presentation` but `pro`'s — its app-scoped
  `ProController` and the page-agnostic paywall a gate raises, both reached through `pro::di`. Other
  app-scoped state or constants live in an app-level module beside
  `failure.rs`/`connection.rs`/`responsive.rs`, reached via context or a plain `crate::` import.

## Tests

`connect.rs`/`sessions.rs`/`terminal.rs`/`settings.rs`/`ui_substrate.rs` exercise the four screens and
the shared `ui/` modules, each mounting its screen beside its own navigator (so it stays visible to
accessibility) under the app's real router; `connect.rs` also scripts the credential mock's prompt
count for "exactly one biometric prompt" per flow. `sessions_loading.rs` is the clock-driven suite
for the loading-rain overlay, standalone and mounted under the real `/sessions` screen. `pro.rs` covers the routeless slice in three bands: the free-tier rule as the pure verdict it
is, `ProController` over the scriptable mock (asserting store *call counts*, the only place the
single-flight guard is observable), and the upgrade sheet over a real overlay host — plus two
whole-app mounts, since the launch refresh is `MuxrApp::init`'s to fire.
`term_widget.rs`/`term_input.rs`/`term_selection.rs` drive the real terminal grid, its keystroke
source and selection/copy model headless against the real `vt100` emulator; `terminal_live.rs` mounts
the whole `/terminal` screen and drives its link-status signal through the "Demo session" cutover.
See [ARCHITECTURE.md](ARCHITECTURE.md#tests) for the shared harness and the cross-cutting suites.
