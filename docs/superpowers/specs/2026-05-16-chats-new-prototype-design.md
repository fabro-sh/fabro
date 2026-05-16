# /chats/new — assistant-ui prototype

Status: design approved, ready for implementation plan.
Owner: fabro-web.
Scope: client-side prototype only. No backend, no persistence.

## Goal

Add a new route `/chats/new` (and `/chats/:id`) to `apps/fabro-web` that delivers a
ChatGPT/Claude-style chat experience inside the existing Fabro AppShell, built on
[assistant-ui](https://www.assistant-ui.com/). The page is positioned as the
eventual replacement for `/start` as Fabro's "kick off agent work" entry point,
but in this phase it is a working mockup with scripted, generic replies — not
wired to any LLM or to Fabro's run engine.

## Non-goals

- No backend integration, no real LLM, no real tool execution.
- No persistence — refreshing the page wipes all chats. No `localStorage`, no
  cookie, no server call.
- No attachments, voice, message editing, branching, regenerate.
- No removal of `/start` in this phase; it stays alongside `/chats/new`. Removal
  happens later, when `/chats/new` is wired to the run engine.
- No tool-call wiring to Fabro's actual tools. The prototype renders generic
  scripted tool-call cards purely for visual demonstration.

## Route and layout

`/chats/new` and `/chats/:id` mount **inside** Fabro's existing `AppShell`,
sibling to `/runs`, `/automations`, etc. The Fabro top nav stays visible.

Routes use AppShell's existing route handles for both `fullHeight: true` (so
the chat surface fills the viewport below the top nav) and `wide: true` (so
the default `max-w-5xl` content constraint is skipped). Both are already
supported by `app/layouts/app-shell.tsx`.

Below the top nav the chat page is a two-column layout:

- **Left chat sidebar** (~260px): "New chat" button at top, then a list of past
  chats (3 hardcoded seeds + any created in-session). No duplicate user menu —
  Fabro's top nav already has it.
- **Main pane**: empty state (centered composer, no message list) on
  `/chats/new`, active conversation on `/chats/:id`.

### Files

- `app/routes/chats.tsx` — layout component for the two-column shell, mounts the
  in-memory chat store via React Context, renders `<Outlet />` for the main pane.
- `app/routes/chats-new.tsx` — empty-state landing.
- `app/routes/chats-detail.tsx` — active conversation view.
- `app/router.tsx` — wires the new nested route group inside the AppShell tree.
- `app/lib/chats-store.tsx` — Context + reducer, in-memory only.
- `app/lib/chats-script.ts` — the scripted reply bank (see Section: Scripted
  replies).
- `app/lib/chats-runtime.ts` — `ChatModelAdapter` factory that consumes the
  store and produces streaming replies for assistant-ui.

## Visual styling

Use assistant-ui's pre-styled components (shadcn-flavored) and override its CSS
variables to align with Fabro's existing palette.

- Install `@assistant-ui/react`, `@assistant-ui/react-ui`, and
  `@assistant-ui/react-markdown`.
- Use `<Thread>` from `@assistant-ui/react-ui` as the message list + composer
  surface. Customize the composer footer via assistant-ui's slot props to inject
  the Fabro chips described below.
- Add a small block to `app/app.css` that maps assistant-ui's CSS variables
  (`--background`, `--foreground`, `--primary`, `--accent`, `--border`,
  `--muted`, `--ring`) to Fabro tokens (page bg, fg, teal, line, etc.).
- Scope the override to a `.fabro-chat` wrapper around the chat layout so the
  shadcn theme cannot bleed into the rest of the app.
- **Verification step during implementation:** assistant-ui's stylesheet is
  authored against Tailwind v3; Fabro uses Tailwind v4. Render the chat once and
  spot-check that critical styles (rounded corners, focus rings, message
  background) come through. If they do not, fall back to importing only
  `@assistant-ui/react` headless primitives and styling them with Fabro tokens
  (more work, kept as a contingency).

## Composer chips (decorative)

Below the textarea the composer footer renders three `@headlessui/react`
`Listbox` buttons styled with Fabro's existing `SECONDARY_BUTTON_CLASS` from
`app/components/ui.tsx`:

- **Project** — hardcoded list: `fabro-web`, `fabro-workflows`, `fabro-cli`.
- **Branch** — hardcoded list: `main`, `develop`, `feature/start-page`.
- **Model** — hardcoded list: `Claude Opus 4.7`, `Claude Sonnet 4.6`, `GPT-5`.

Selections live in component state and update the chip label, but **do not
affect** the scripted reply. Project/branch values match those already seeded in
`app/routes/start.tsx` for visual continuity.

## Data model

The prototype reuses the existing TS API client types verbatim. No new message
types are introduced.

```ts
import type {
  CompletionMessage,
  CompletionContentPart,
} from "@qltysh/fabro-api-client";
```

- `CompletionMessage` — `{ role, content, name?, tool_call_id? }`, with `role`
  enum `system | user | assistant | tool | developer`.
- `CompletionContentPart` — `{ kind, data }`, where `kind` is `text`,
  `tool_call`, `tool_result`, `thinking`, `image`, etc.

These match what `fabro-llm`, the OpenAPI spec, and (serialized)
`fabro_agent::Turn` already use. Scripted replies are written directly as
arrays of `CompletionMessage`, so the bank is wire-shaped from day one.

### Only new type: a chat-collection wrapper

The codebase does not have a user-facing "conversation in a list" type;
`Session` already means an auth session or a Rust runtime session. The prototype
adds one tightly-scoped local type:

```ts
type Chat = {
  id: string;                       // e.g. "c_a1b2"
  title: string;                    // derived from first user message
  createdAt: number;
  messages: CompletionMessage[];
  scriptIndex: number;              // prototype-only; points into reply bank
};

type ChatStore = {
  chats: Record<string, Chat>;
  order: string[];                  // sidebar order, newest first
  createChat(): string;             // returns new id
  appendUserMessage(id: string, text: string): void;
  appendAssistantReply(id: string, reply: CompletionMessage): void;
};
```

Store lives in `app/lib/chats-store.tsx` (Context + `useReducer`). No
persistence.

### assistant-ui adapter boundary

assistant-ui's runtime uses its own internal `ThreadMessage` shape. Conversion
between `CompletionMessage[]` and `ThreadMessage[]` happens in exactly one
place: the `ChatModelAdapter` factory in `app/lib/chats-runtime.ts`. This
mirrors the OpenAPI/internal-type boundary pattern already established in the
top-level `CLAUDE.md` ("API DTOs are projections; convert at the boundary").

Streaming chunks stay inside assistant-ui's runtime. The store only sees the
completed reply, written back as a single `CompletionMessage`. This keeps the
canonical message model free of simulated-streaming state.

## Scripted replies

`app/lib/chats-script.ts` exports a `CompletionMessage[]` of length ~6, all
with `role: "assistant"`. Content parts mix `text` (markdown), `tool_call`,
and matching `tool_result`. The bank is generic, not Fabro-specific:

1. Plain markdown paragraph (greeting + short list).
2. Markdown with a fenced TypeScript code block.
3. Reply containing one `tool_call` + `tool_result` pair — generic
   `search_web` with mock args and a short result.
4. Long markdown with headings and a blockquote.
5. Reply containing one `tool_call` + `tool_result` pair — generic
   `run_calculation`.
6. Short reply mentioning a "next step".

Each seed chat starts at a staggered `scriptIndex` so the sidebar does not look
uniform.

### Seed data

Three hardcoded past chats are inserted into the store on first mount, so the
sidebar is populated on initial page load:

- "Draft a launch email"
- "Refactor a React hook"
- "Compare Postgres vs SQLite"

Each has 2-3 messages pre-populated from the reply bank.

### Title derivation

When a new chat's first user message is sent, the chat title is set to the
first ~40 characters of that message's first `text` content part (trimmed at a
word boundary if convenient, otherwise hard-truncated with an ellipsis).

## Behavior

1. First visit to `/chats/new` renders the empty state: centered composer, no
   message list, sidebar shows seed chats.
2. User types and sends:
   - Reducer creates a new `Chat`, appends the user `CompletionMessage`,
     navigates to `/chats/:id`.
3. The per-chat `ChatModelAdapter` fires:
   - Reads `scriptIndex` from the store, picks `bank[scriptIndex % bank.length]`.
   - Simulates streaming by yielding `text` content parts in ~30-char chunks
     every ~60ms via `setTimeout`. `tool_call` and `tool_result` parts emit as
     single chunks.
4. On stream completion the reducer appends the full `CompletionMessage` to
   the chat and bumps `scriptIndex` by one.
5. Clicking a chat in the sidebar navigates to `/chats/:id`; the main pane
   swaps the runtime instance; history is preserved in memory.
6. Clicking "New chat" navigates back to `/chats/new`; composer state resets.

## Error handling

Minimal — there is no network and no LLM.

- Empty/whitespace-only sends are ignored (assistant-ui composer's built-in
  behavior).
- Navigating to `/chats/:id` for an unknown id renders an empty state with a
  "Start a new chat" link (no crash, no redirect loop).
- The scripted adapter never throws. If the bank is somehow empty, render a
  one-line fallback assistant message.

## Testing

Proportionate to a UI prototype, using existing `bun test` and
`react-test-renderer` patterns in `apps/fabro-web`.

1. `chats-router.test.tsx` — `/chats/new` and `/chats/:id` resolve to the
   right components inside the AppShell tree.
2. `chats-store.test.tsx` — reducer: create, append user message, append
   assistant reply, `scriptIndex` advance and wrap, title derivation from a
   long input, title derivation from input with leading whitespace.
3. `chats-script-adapter.test.ts` — given a `Chat`, the adapter returns the
   next bank entry as a streaming sequence in the correct order and advances
   `scriptIndex` exactly once per send.

No snapshot tests for assistant-ui's rendered DOM — too volatile, low signal.

## Verification before declaring done

- `cd apps/fabro-web && bun run typecheck` — clean.
- `cd apps/fabro-web && bun test` — green.
- `fabro server start` and `cd apps/fabro-web && bun run dev`, then a manual
  browser walkthrough:
  - `/chats/new` renders empty state with Fabro top nav visible.
  - Sending a message navigates to `/chats/:id`, the reply streams in, and a
    tool-call card renders correctly for the relevant bank entries.
  - Switching between seed chats in the sidebar preserves message history.
  - Creating a new chat from the sidebar returns to the empty state.
  - Composer chips open popovers and update their labels.
  - Chat surface fills viewport height; AppShell top nav stays put.

## Open items deferred to follow-up work

- Wiring the composer chips to actual project/branch/model state, and to a real
  agent runner.
- Replacing scripted replies with a real LLM stream (likely via the existing
  fabro-api completions endpoint).
- Deciding how chats persist server-side and how they relate to runs.
- Removing `/start` once `/chats/new` is fully wired.
- Tool-call rendering for Fabro's actual tools (file ops, bash, etc.).
- Attachments, voice, branching, regenerate.
