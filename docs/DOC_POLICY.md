# Doc Policy

**Set by:** agent
**Scale:** small (~4.6k source LOC in `src/`, 1 package, 1 stack (Rust + Android/iOS shells) → 1 doc unit)
**Structure:** flat

## Budgets

| Doc | Target | Cap |
|-----|--------|-----|
| `docs/ARCHITECTURE.md` | 135 | 150 |
| `docs/DEVELOPMENT.md` | 90 | 120 |
| `CLAUDE.md` | 60 | 80 |

Tighter than the generic small-tier schema caps (250/400 core doc, 60/80 `CLAUDE.md`) — this is a
single-crate app, not a workspace, and the reading path stays short deliberately.

`docs/ARCHITECTURE.md`'s target was raised from 110 to 135 on 2026-07-31: its "Building a Screen
(canonical pattern)" section is permanent, load-bearing content (the app-structure charter every
screen and, post-open-sourcing, every frust app author is meant to copy), not transient bloat to
compact away. A 2026-07-31 docs-close pass folded in the session guard/`Nav` seam, the `connection`
module, three new `ui/` modules, and two new charter bullets — real content, not bloat — compacting
Shell/Tests/Design References to absorb it; landed at 148/150, still no override needed.

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
