# Doc Policy

**Set by:** agent
**Scale:** medium (~23.5k source LOC in `src/`, 1 package, 1 stack (Rust + Android/iOS shells) →
1 doc unit) — crossed the 15k small→medium boundary as all four feature slices (`connect`,
`sessions`, `terminal`, `settings`) landed as full `{domain, data, presentation}` slices.
**Structure:** flat — still one unit; the tier crossing changes the budget, not the shape.

## Budgets

| Doc | Target | Cap |
|-----|--------|-----|
| `docs/ARCHITECTURE.md` | 220 | 260 (320 — see Recorded Overrides) |
| `docs/DEVELOPMENT.md` | 100 | 130 (180 — see Recorded Overrides) |
| `CLAUDE.md` | 65 | 85 |

Tighter than the generic medium-tier schema caps (350/500 core doc, 80/100 `CLAUDE.md`) — this is a
single-crate app, not a workspace, and the reading path stays short deliberately.

`docs/ARCHITECTURE.md`'s target was raised from 135 to 220 on 2026-08-01 as real content grew
(Shell/Navigation/UI Substrate/Feature Slices, Responsive). The terminal widget build (tasks 08–11)
landed it at 260/260 exactly, offsetting new-subsystem content with trims elsewhere in the same pass
and flagging that the *next* addition of any size would need a split rather than another trim.

That addition landed as p5-12 (2026-08-07): Phase 5 wired the real `muxrd` gRPC layer — `src/net`, a
rebuilt `connection.rs`, and real repositories across all four feature slices — in one pass, per an
explicit instruction to update `docs/ARCHITECTURE.md`/`docs/DEVELOPMENT.md` in place rather than
restructure. Both docs carry a bounded, time-limited **Recorded Override** below as a result (see
that section) instead of the split the previous note anticipated. **The split is now due, not just
anticipated** — the next dispatch that touches either doc should escalate to hub-and-spoke (spoke
candidates: `docs/NET_ARCHITECTURE.md` covering `src/net`/`connection.rs`, or a per-feature split)
rather than compact further or extend either override.

## Doc Units

Single unit, the whole app — no split; `docs/ARCHITECTURE.md` and `docs/DEVELOPMENT.md` cover it
whole. See the override note above: this is the trigger for a hub-and-spoke escalation on the next
touch, not a durable position.

`docs/CODE_STANDARDS.md` and `docs/REVIEW_FOCUS.md` do not exist yet — created on demand (per the
doc-maintainer's "no empty stubs" rule) once there is project-specific content to put in them;
until then this app inherits the parent Frust checkout's `docs/CODE_STANDARDS.md`.

**Excluded** (design references, not doc units — own README/index line only): `docs/*.html`
(interactive mocks), `docs/muxr-app-icons/` (icon masters).

## Recorded Overrides

- `docs/ARCHITECTURE.md` → cap raised **260 → 320** (still under the +50% schema ceiling). p5-12
  documented `src/net`, a rebuilt `connection.rs`, and Phase 5's real-gRPC facts across all four
  feature slices in one pass rather than splitting, per that task's explicit "update in place, don't
  restructure" instruction; compaction alone (trimming the Planned section's shipped items, deduping
  the Net/Tests cross-reference, tightening Feature Slices prose) recovered content but landed at
  312/320; the phase-5 review fix rounds (layer-table true-ups, the deferred shared-channel
  limitation note, the layering-suite Tests entry) then consumed the remaining slack — the file
  now sits at **320/320, the hard cap exactly, zero headroom**. **The re-check trigger has
  FIRED:** the next `docs/ARCHITECTURE.md` edit of any size cannot land in place — split into
  hub-and-spoke first; do not raise this cap again.
- `docs/DEVELOPMENT.md` → cap raised **130 → 180**. Already over its recorded cap (152) going into
  p5-12 from an earlier untracked addition (the Codegen/crypto-provider section for `src/net`, added
  without a matching policy update); p5-12 added a "Test Rig (Live `muxrd`)" section on top, landing
  at 175/180. **Re-check trigger:** the next `docs/DEVELOPMENT.md` edit of any size — compact the
  Prerequisites/Signing sections or split before raising this further.
