# Doc Policy

**Set by:** agent
**Scale:** medium (~23.5k source LOC in `src/`, 1 package, 1 stack (Rust + Android/iOS shells) →
1 doc unit) — crossed the 15k small→medium boundary as all four feature slices (`connect`,
`sessions`, `terminal`, `settings`) landed as full `{domain, data, presentation}` slices.
**Structure:** flat — still one unit; the tier crossing changes the budget, not the shape.

## Budgets

| Doc | Target | Cap |
|-----|--------|-----|
| `docs/ARCHITECTURE.md` | 220 | 260 |
| `docs/DEVELOPMENT.md` | 100 | 130 |
| `CLAUDE.md` | 65 | 85 |

Tighter than the generic medium-tier schema caps (350/500 core doc, 80/100 `CLAUDE.md`) — this is a
single-crate app, not a workspace, and the reading path stays short deliberately.

`docs/ARCHITECTURE.md`'s target was raised from 135 to 220 on 2026-08-01 as real content grew
(Shell/Navigation/UI Substrate/Feature Slices, Responsive). The terminal widget build (tasks 08–11)
landed it at 260/260 exactly, offsetting new-subsystem content with trims elsewhere in the same pass.
At the cap: the next addition of any size needs a split (hub-and-spoke, a `terminal`-specific doc)
rather than another trim pass — re-check then, or on a fifth feature.

## Doc Units

Single unit, the whole app — no split; `docs/ARCHITECTURE.md` and `docs/DEVELOPMENT.md` cover it
whole.

`docs/CODE_STANDARDS.md` and `docs/REVIEW_FOCUS.md` do not exist yet — created on demand (per the
doc-maintainer's "no empty stubs" rule) once there is project-specific content to put in them;
until then this app inherits the parent Frust checkout's `docs/CODE_STANDARDS.md`.

**Excluded** (design references, not doc units — own README/index line only): `docs/*.html`
(interactive mocks), `docs/muxr-app-icons/` (icon masters).

## Recorded Overrides

None.
