# Factory components in Fabro

Factory's stylesheet → `factory-components.css` → the constants in `components/ui.tsx` → every Fabro screen that uses them.

Colors, radii, shadows and fonts come from `factory-themes.css` (see `FACTORY_THEMES.md`). This file holds shapes and states only. A test fails if a palette color lands in it.

**Origin:** adapted from the owner's private Factory UI (`ui/src/styles/globals.css`, `ui/src/components/ui/index.tsx`, `ui/src/features/process/ProcessPanel.tsx`, `ui/src/lib/process/model.ts`). Owner-authored, reused on the owner's authorization. Not third-party code and not under the MIT notices in `public/THIRD_PARTY_NOTICES.txt`.

**Runtime:** no dependency on Factory's server, data, boards or files. Fabro's own data drives everything.

## One-to-one with Factory

Same class names, sizes and states:

- Buttons: `.btn`, `.btn-sm`, `.btn-primary`, `.btn-signal`, `.btn-ghost`, `.btn-danger`, `.link`
- Fields: `.input`, `.field-label`
- Surfaces: `.card`, `.raised`, `.modal`, `.menu`, `.menu-item`, `.scroll-pane`, `.hatch`
- Labels: `.eyebrow`, `.chip` and its tones, `.glyph` and its states
- Tabs: `.folder-tabs`, `.folder-tab` (ready, not yet used by a Fabro screen)
- Timeline: 24px rows, 16px bars, 12px dashed bands for waiting, the running stripes, the open bracket for a start with no finish, hatched folded breaks, the now line, the words-only table for screen readers
- Rule: a bar needs both ends recorded, or work still running. Anything else is a marker or words, never an estimate.

`.btn-primary` reads `--primary`, so Signal is navy. `.btn-signal` reads `--accent-pop`. Use signal only where Factory itself has a signal call to action.

## Fabro's own, in Factory's look

Fabro needs these and Factory has no equivalent, so they are built from Factory's tokens:

- `.tooltip-surface`, `.popover-surface`: Factory uses native title text. Fabro keeps Radix tooltips and hover cards.
- `ConfirmDialog`: Factory confirms inline. Fabro keeps its HeadlessUI dialog (focus trap, Escape) in `.modal`, with `.modal-confirm` narrowing it to 448px.
- `.menu-item-danger`, `.input:disabled`, `.input[aria-invalid]`, `.btn[data-filtering]`, `.check`.
- Keyboard focus stays inside control boundaries and preserves their shape. Pointer-only
  focus has no outline; focused canvas nodes strengthen their own shape stroke.
- `Spinner`: Factory writes "Loading…" with no spinner. Fabro keeps its spinner.
- Runs table: Factory's issue list is cards. Fabro keeps its table, with Factory's table type, row rules, chips and selection marker.

## Timeline: what is adapted

Factory's Timeline shows clock times for one ticket's workflow steps. Fabro's shows elapsed time for one run's stages.

| Factory | Fabro | Why |
| --- | --- | --- |
| Clock labels on the axis | Elapsed labels (`0s`, `5m`) | Fabro's existing time model |
| Agent windows row | Run lifecycle group | Same "waiting, not running" band, on Fabro's phases |
| Fixed 700px track | Fills the panel, 560px minimum, scrolls | Stays usable on a narrow canvas |
| Folds any 30-minute jump between recorded times | Folds only 30 minutes no row covers | A stage that truly ran 5 hours keeps its width |
| No zoom | Fit, zoom in, zoom out (1x to 16x), keys `+` `-` `0` | Long runs need detail |
| `×N` attempts | `name@N` label | Fabro's stage-reference syntax |
| Bar is the click target | Whole row is the link | Fabro's existing behavior |
| Server-anchored clock | Browser clock, elapsed never below zero | Fabro's stage API sends no server time |

Other deliberate differences from Factory's literals: empty and error states use `--radius-md` where Factory hard-codes 14px and 12px, and bars cap at `min(4px, --radius-xs)`, so Brutalist stays square. Inputs go to 16px under 640px wide, because iOS zooms into anything smaller.

## Mechanics kept

HeadlessUI menus, listboxes and dialogs. Radix tooltips and hover cards. Heroicons, not Lucide. No new dependencies.

## Layering

Everything sits in `@layer components`, so a Tailwind utility passed by a caller still wins: `` `${INPUT_CLASS} font-mono` ``, `` `${PRIMARY_BUTTON_CLASS} w-full` ``.
