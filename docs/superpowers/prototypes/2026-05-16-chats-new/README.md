# chats-new + Ask-Fabro prototype

Isolated, client-side prototype validating the design for the upcoming
`/chats/new` page and the right-sidebar "Ask Fabro" assistant in fabro-web.

**Status:** reference implementation. Not deployed; not part of the build.
Lives here so the visual and integration learnings survive until they land in
`apps/fabro-web`.

**Companion spec:** `../../specs/2026-05-16-chats-new-prototype-design.md`

## Run it

```bash
cd docs/superpowers/prototypes/2026-05-16-chats-new
bun install
bun run dev
```

Then open <http://127.0.0.1:5173/>.

## Routes

- `/chats/new` — full-page ChatGPT-style empty composer.
- `/chats/:id` — full-page chat thread with sidebar of past chats.
  Seeded with three example threads (`seed_email`, `seed_hook`, `seed_db`).
- `/sample` — demo workspace page with an "Ask Fabro" button in the header that
  toggles a right-side assistant sidebar.

## What it proves

- assistant-ui's `<Thread>` + `useLocalRuntime` integrate cleanly with React 19,
  React Router 7, Tailwind v4, and the Fabro color palette.
- A custom `ChatModelAdapter` can stream scripted replies (text + tool calls)
  through assistant-ui's runtime.
- The `@layer assistant-ui` import wrap is required for Tailwind v4 utilities
  to win over assistant-ui's unlayered scoped preflight. See spec.
- A right-sidebar assistant pattern works by rendering `<Thread>` inside a
  fixed-height side panel with `--aui-thread-max-width: 100%` and a stripped
  composer (no chips, single-line, rounded pill).
- Sidebar customizations needed for a narrow column: hide avatar, hide
  action bar, override `align-items: stretch` on the viewport footer,
  `margin-top: auto` so the composer pins to the bottom in the empty state,
  `padding-right` on assistant message content to keep code blocks inside
  the column without `<pre>` losing its right padding to overflow.

## What it does NOT prove

- No backend integration. All replies come from `src/lib/chats-script.ts`.
- No persistence. Refreshing the page wipes new chats.
- No real auth, no real project/branch/model selection (chips are decorative).
- The mock top-nav links other than `Sample` are dead.

## Porting to fabro-web

When the integration plan is written, key files to port (with adaptations):

- `src/index.css` — the `@layer assistant-ui` block and `.fabro-chat` /
  `.ask-fabro-sidebar` override blocks. Drop the local Fabro theme tokens
  (already in `apps/fabro-web/app/app.css`).
- `src/components/{composer-chips,custom-composer,sidebar-composer,tool-fallback,ask-fabro-sidebar}.tsx`
  — copy and adapt to fabro-web conventions (use existing UI helpers from
  `apps/fabro-web/app/components/ui.tsx`).
- `src/lib/chats-runtime.ts` (the `ChatModelAdapter` + `toThreadMessages`
  conversion) — replace the scripted bank with a real completions client.
- `src/lib/ask-fabro-context.tsx` — lift state into fabro-web's AppShell.
- `src/routes/chats-{layout,new,detail}.tsx` and `src/routes/sample.tsx`
  — adapt to fabro-web's `app/routes/` and `app/router.tsx`.
