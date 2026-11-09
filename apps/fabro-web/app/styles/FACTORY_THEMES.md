# Factory themes in Fabro

Fabro's seven palettes are Factory's. `factory-themes.css` preserves its palette declarations, with a shorter source comment.
The palettes need no other install, service or machine to build or run.

## Source

| File here | Factory source | SHA-256 of the source |
| --- | --- | --- |
| `factory-themes.css` | `ui/src/styles/themes.css` | `aae81988a216d0d16e7d617b041747d5add219f2216734cbb527ab086b2cb11e` |
| `THEMES` and `DISPLAY_MODES` in `app/lib/appearance.ts` | `ui/src/lib/theme.ts` | `75faf69d6743f4e8bfa256691ec521a71cb3dd387785c4e7adeb1941652b8bc3` |
| Chooser in `app/routes/settings-appearance.tsx` | `AppearanceDialog` in `ui/src/features/dialogs/Dialogs.tsx`, and `.theme-choice` in `ui/src/styles/globals.css` | adapted, not copied |
| `factory-fonts.css` | `@font-face` rules in `ui/src/styles/globals.css` | paths changed only |
| `public/assets/fonts/*.woff2` | `ui/src/fonts/` | byte-identical |
| `public/licenses/ofl-*.txt` | `ui/public/licenses/` | byte-identical |

Adapted on 2026-11-08 from the author's Factory application. The source hashes above
record the inputs; Fabro's edited CSS snapshot has SHA-256 `0af016dfa47cf43d64cc6bc6864a2427c06c6c43213f3377c0e80339750f198b`.

`appearance.test.ts` fails if `factory-themes.css` changes by a single byte, or if the
generated files fall behind it. The other Factory files aren't read at test time. Compare
them by hand when updating.

## What differs from Factory on purpose

- **Storage.** Factory keeps the choice in `localStorage` under `factory:appearance`. Fabro
  uses its own cookie, `fabro-appearance`, and never reads Factory's or the Internal Hub's
  keys. Cookies ignore ports, so a shared name would have leaked choices across apps.
- **Default.** Fabro starts in Brutalist light mode. Explicit saved choices still win;
  Signal navy remains Factory's fallback palette for unthemed markup.
- **The display mode control** uses native radios rather than buttons with `role="radio"`,
  which adds arrow-key support.

## How Fabro's tokens map

Every value is a Factory token, or derived from them where Fabro's markup needs a role
Factory doesn't have. `appearance.test.ts` measures contrast through these real mappings
on all 14 palettes. The mappings live in `app/app.css` (`--color-*`) and
`styles/appearance.css` (the rest).

| Fabro | Factory | Note |
| --- | --- | --- |
| `--color-page`, `panel`, `panel-alt` | `--bg`, `--surface`, `--surface-2` | exact |
| `--color-fg`, `fg-2`, `fg-muted` | `--fg`, `--fg-2`, `--muted` | exact |
| `--color-fg-3` | 50% `--fg-2`, 50% `--muted` | **derived**: Fabro has one more text level |
| `--color-overlay`, `overlay-strong` | 5% and 11% `--fg` | **derived**: translucent hover washes |
| `--color-line`, `line-strong`, `focus` | `--line`, `--line-strong`, `--ring` | exact |
| `--color-primary`, `primary-hover`, `on-primary` | `--primary`, `--primary-hover`, `--primary-fg` | exact |
| `--color-teal-500`, `teal-700` | `--accent-ink`, `--accent-fill` | exact. The old utility name now means accent |
| `--color-teal-300` | 88% `--accent-soft-ink`, 12% `--fg` | **derived**: a stronger accent text |
| `--color-mint`, `amber`, `coral` | `--ok`, `--warn`, `--bad` | exact |
| `--diagram-paper`, `surface`, `ink` | `--bg`, `--surface`, `--fg` | exact |
| `--diagram-muted`, `soft`, `edge` | `--muted` | exact. Factory has one muted role |
| `--diagram-accent`, `accent-ink`, `accent-tint` | `--accent-fill`, `--accent-ink`, `--accent-soft` | exact |
| `--diagram-success`, `danger`, `warning`, and `-tint` | `--ok`, `--bad`, `--warn`, and `-bg` | exact |
| `--diagram-rule`, `grid` | `--line`, `--hatch` | exact |
| `--diagram-node-radius` | `--radius-xs` | exact |
| `--nav-*` | `--rail-*` | exact. `--nav-selected-border` reads `--rail-indicator` |
| `--chart-series-1` to `5` | `--accent-fill`, `--info`, `--ok`, `--warn`, `--violet` | **derived**: Factory has no series palette |
| `--diffs-*-override` | surfaces, `--muted`, `--ok`, `--bad` | exact |
| `--aui-*` | see `scripts/factory-themes.ts` | channels computed from exact tokens |
| xterm theme | `terminalColorsFor()` in `app/lib/appearance.ts` | **derived**: Factory has no terminal palette |

Every Factory token (`--rail-*`, `--e1`, `--radius-btn`, `--accent-pop` and the rest) is also
available under its own name, and Factory's Tailwind names (`bg-surface`, `text-muted`,
`bg-ok-bg`, `shadow-e2`) work as utilities.

## Updating

1. Copy Factory's `ui/src/styles/themes.css` over `factory-themes.css`.
2. Keep the public source comment concise, then update the snapshot hash in
   `app/lib/appearance.test.ts` and this document. Record the input hash in the table above.
3. Run `bun run scripts/factory-themes.ts` from `apps/fabro-web`.
4. Run `bun test --isolate app/lib/appearance.test.ts app/lib/graph-theme.test.ts`.
