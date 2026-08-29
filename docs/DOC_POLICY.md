# Doc Policy

**Set by:** agent
**Scale:** medium (~54.5k source LOC in `src/`, 1 package, 1 stack → 3 architecture units + 1
custom doc) — grew from ~23.5k (Phase-5 gRPC + terminal/settings arcs) and again with the `pro`
slice; still under the 75k medium→large boundary.
**Structure:** hub-and-spoke. `docs/ARCHITECTURE.md`'s 320/320 hard cap fired the split trigger
(2026-08-15, presentation-cleanup refactor): NET and FEATURES pulled out as spokes; the hub keeps
composition root, shell, navigation, UI substrate, responsive, tests, design refs, planned.

## Budgets

| Doc | Target | Cap |
|-----|--------|-----|
| `docs/ARCHITECTURE.md` (hub) | 180 | 220 |
| `docs/NET_ARCHITECTURE.md` | 90 | 120 |
| `docs/FEATURES_ARCHITECTURE.md` | 160 | 200 |
| `docs/DEVELOPMENT.md` | 100 | 130 (180 — see Recorded Overrides) |
| `docs/RELEASE_ANDROID.md` | 130 | 160 |
| `CLAUDE.md` | 65 | 85 |

Tighter than the generic medium-tier schema caps — deliberately short reading path, single crate.

## Doc Units

| Unit | Covers | Doc path |
|------|--------|----------|
| (hub) | Composition, shell, navigation, UI substrate, responsive, tests, planned | `docs/ARCHITECTURE.md` |
| `NET` | `src/net/`, `src/connection.rs` — gRPC transport + connection state | `docs/NET_ARCHITECTURE.md` |
| `FEATURES` | `src/features/*` — the five slices + the canonical screen-building pattern | `docs/FEATURES_ARCHITECTURE.md` |
| `RELEASE_ANDROID` (custom) | Play Console entries, products, license testers, tracks, the upload/release checklist — the human half `docs/DEVELOPMENT.md` points at | `docs/RELEASE_ANDROID.md` |

`CODE_STANDARDS.md`/`REVIEW_FOCUS.md` don't exist yet (no empty stubs); this app inherits the
parent Frust checkout's `docs/CODE_STANDARDS.md` until one is warranted.

**Excluded** (own README/index line only, not doc units): `docs/*.html` mocks,
`docs/muxr-app-icons/`.

## Recorded Overrides

- `docs/DEVELOPMENT.md` → cap raised **130 → 180**, carried over unchanged this pass. Sits at
  180/180: the IAP/release pass compacted Prerequisites, Codegen, the Test Rig and Signing to pay
  for `mock-iap`, flavor billing, `[signing]` and Release (Play). **Re-check trigger:** the next
  edit of any size — there is no headroom left, so split the Android signing/release half into its
  own spoke rather than raise this cap again.

## Pressure Watch

- `docs/ARCHITECTURE.md` (218/220) and `docs/FEATURES_ARCHITECTURE.md` (200/200) both sit at cap
  after the `pro` slice landed, each well past its target. Whichever is edited next splits rather
  than trades away more prose: the hub's `UI Substrate` section into `docs/UI_ARCHITECTURE.md`, and
  FEATURES along a slice boundary (`terminal` is the largest bullet by far).
