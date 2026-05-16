# /chats/new + /chats/:id — fabro-web port implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the validated `/chats/new` + `/chats/:id` prototype (ChatGPT-style chat surface with seeded past chats, scripted replies, project/branch/model composer chips) into `apps/fabro-web`, mounted inside the existing AppShell. Scope is client-side only — no backend, no real LLM. The right-side Ask-Fabro assistant is explicitly out of scope for this plan.

**Architecture:** Mirror the prototype's structure. Routes mount inside fabro-web's `AppShell` (use route `handle` exports for `fullHeight: true` and `wide: true`). The Thread component from `@assistant-ui/react-ui` renders messages and the composer; `useLocalRuntime` runs a custom `ChatModelAdapter` that streams scripted replies from a fixed reply bank. The chat-collection metadata lives in a React Context + reducer; the runtime owns message state per chat. Tailwind v4 utilities are made to win over assistant-ui's scoped preflight by declaring layer order and importing assistant-ui CSS into `@layer assistant-ui`.

**Tech Stack:** React 19, React Router 7, Tailwind v4, HeadlessUI, Heroicons, `@assistant-ui/react`, `@assistant-ui/react-ui`, `@assistant-ui/react-markdown`. Build with Bun (not Vite — fabro-web uses a custom Bun bundler at `apps/fabro-web/scripts/build.ts`). Tests with `bun:test` + `react-test-renderer`.

**Reference implementation:** `docs/superpowers/prototypes/2026-05-16-chats-new/` is a complete, runnable Vite prototype of every component below. Copy and adapt — do not rebuild from scratch. The companion spec is `docs/superpowers/specs/2026-05-16-chats-new-prototype-design.md`.

**Known issue this plan addresses:** The prototype disabled React StrictMode to work around an autorespond race (the `runtime.thread.append(pendingText)` effect would lose its stream when StrictMode double-invokes mount). fabro-web uses StrictMode in production (`apps/fabro-web/app/entry.tsx`). This plan keeps StrictMode and routes the first-message handoff through the chat store rather than via a programmatic `runtime.thread.append` from a useEffect, eliminating the race.

---

## File structure

| File | Responsibility |
|---|---|
| `apps/fabro-web/package.json` | Add `@assistant-ui/react`, `@assistant-ui/react-ui`, `@assistant-ui/react-markdown` deps |
| `apps/fabro-web/app/app.css` | Add cascade `@layer` declaration, assistant-ui CSS imports into `@layer assistant-ui`, `.fabro-chat` `--aui-*` variable overrides |
| `apps/fabro-web/app/lib/chats-types.ts` | Local discriminated-union `ChatCompletionContentPart` (assignable to the API client's `CompletionContentPart`) + `Chat` store wrapper type |
| `apps/fabro-web/app/lib/chats-script.ts` | Scripted reply bank: `CompletionMessage[]` of length 6 (text, code, tool-call+result, long markdown, calc tool-call, short reply) |
| `apps/fabro-web/app/lib/chats-store.tsx` | React Context + `useReducer`. Holds chat metadata (id, title, createdAt, scriptIndex, `seedMessages`, `pendingFirstMessage`). No persistence |
| `apps/fabro-web/app/lib/chats-store.test.tsx` | Reducer unit tests |
| `apps/fabro-web/app/lib/chats-runtime.ts` | `createScriptedAdapter()` factory returning a `ChatModelAdapter`; `toThreadMessages()` converter from `CompletionMessage[]` to assistant-ui's `ThreadMessageLike[]` |
| `apps/fabro-web/app/lib/chats-runtime.test.ts` | Adapter streaming + scriptIndex advance tests |
| `apps/fabro-web/app/components/chats/tool-fallback.tsx` | `ToolFallback` renderer for tool calls (header + arguments + result) |
| `apps/fabro-web/app/components/chats/composer-chips.tsx` | Decorative project / branch / model chips using `@headlessui/react` Listbox |
| `apps/fabro-web/app/components/chats/custom-composer.tsx` | Composer component using `ComposerPrimitive`, rounded card, chips inside, send + stop buttons |
| `apps/fabro-web/app/routes/chats-layout.tsx` | Two-column shell (chat sidebar + outlet); mounts `ChatsProvider`; route handle: `{ fullHeight: true, wide: true }` |
| `apps/fabro-web/app/routes/chats-new.tsx` | Empty state: composer centered. On submit, creates a chat with first user message + navigates to `/chats/:id` |
| `apps/fabro-web/app/routes/chats-detail.tsx` | Active conversation: `useLocalRuntime` with `initialMessages` from the store's chat history; renders `<Thread>` with `CustomComposer`, markdown text, `ToolFallback` |
| `apps/fabro-web/app/router.tsx` | Register chats routes under the AppShell route tree |
| `apps/fabro-web/app/routes/chats-router.test.tsx` | Resolves `/chats/new` and `/chats/:id` inside AppShell |

---

## StrictMode-safe first-message handoff (architectural note)

The prototype passes the first user message via `location.state.pendingText` and calls `runtime.thread.append(pendingText)` inside a `useEffect`. Under StrictMode that effect runs, cleanup aborts the in-flight stream, then the effect runs again with the ref already true — leaving an empty assistant placeholder.

**This plan's pattern:**

1. `chats-new` calls `store.createChatWithFirstMessage(text)` — the store immediately seeds the chat with the user message as part of `seedMessages`, *and* sets a `pendingResponse: true` flag.
2. `chats-new` navigates to `/chats/:id` (no router state needed).
3. `chats-detail` mounts, `useLocalRuntime` initialises with `initialMessages = toThreadMessages(chat.seedMessages)`. The user message is already in the runtime's history.
4. `chats-detail` reads `chat.pendingResponse`. If true, it calls `runtime.thread.startRun({ parentId: null })` once via `useEffect`, then dispatches `consumePendingResponse(chatId)` which sets the flag false.

`startRun` (idempotent — only the most recent run survives), combined with store-level deduplication (`pendingResponse: false` after consume), means StrictMode's mount-cleanup-mount cycle is safe: the second run after cleanup is the one that completes and writes back to the store.

---

### Task 1: Add assistant-ui dependencies

**Files:**
- Modify: `apps/fabro-web/package.json`

- [ ] **Step 1: Add the three assistant-ui packages**

Edit `apps/fabro-web/package.json` and insert the following into the `"dependencies"` block (keep alphabetical ordering):

```json
"@assistant-ui/react": "0.14.5",
"@assistant-ui/react-markdown": "0.14.0",
"@assistant-ui/react-ui": "0.2.1",
```

- [ ] **Step 2: Install**

Run: `cd apps/fabro-web && bun install`
Expected: succeeds; three new entries appear in `bun.lock`.

- [ ] **Step 3: Verify the build still works**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes (no new errors). The packages are not imported yet, just installed.

- [ ] **Step 4: Commit**

```bash
git add apps/fabro-web/package.json apps/fabro-web/bun.lock
git commit -m "deps(fabro-web): add @assistant-ui/{react,react-ui,react-markdown}"
```

---

### Task 2: Layer-wrap assistant-ui CSS in app.css

**Files:**
- Modify: `apps/fabro-web/app/app.css`

Why first: this is the foundational cascade fix. Without it, Tailwind v4 utilities silently lose to assistant-ui's scoped preflight inside `.aui-root`, and headings inside Thread (welcome screen, tool-fallback) render at base font-size. Verified empirically in the prototype.

- [ ] **Step 1: Add the cascade layer order + assistant-ui imports**

Open `apps/fabro-web/app/app.css`. Change the top of the file from:

```css
@import "@xterm/xterm/css/xterm.css";
@import "tailwindcss";
@plugin "@tailwindcss/typography";
```

to:

```css
@import "@xterm/xterm/css/xterm.css";

/*
 * Cascade layer order. @assistant-ui/react-ui ships unlayered CSS authored
 * against Tailwind v3; in Tailwind v4 utilities live in @layer utilities, and
 * any unlayered CSS wins over layered CSS regardless of selector specificity.
 * Putting assistant-ui in a named layer that we declare BEFORE utilities makes
 * Tailwind v4 utility classes cascade above assistant-ui's scoped preflight.
 */
@layer theme, base, assistant-ui, components, utilities;

@import "tailwindcss";
@plugin "@tailwindcss/typography";

@import "@assistant-ui/react-ui/styles/index.css" layer(assistant-ui);
@import "@assistant-ui/react-ui/styles/markdown.css" layer(assistant-ui);
```

- [ ] **Step 2: Verify the bundler resolves the imports**

Run: `cd apps/fabro-web && bun run build`
Expected: build succeeds; no missing-file errors for the assistant-ui CSS paths.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/app.css
git commit -m "style(fabro-web): wrap assistant-ui CSS in @layer for Tailwind v4 cascade"
```

---

### Task 3: Add `.fabro-chat` theme overrides

**Files:**
- Modify: `apps/fabro-web/app/app.css`

- [ ] **Step 1: Append the override block**

Append to the end of `apps/fabro-web/app/app.css`:

```css
/* ---------------------------------------------------------------------------
 * assistant-ui theme overrides (--aui-* variables) — mapped to Fabro tokens.
 * Values are HSL component triples so assistant-ui's hsl(var(...)) wrapper
 * works. Scoped to .fabro-chat so the shadcn theme cannot leak out.
 *
 * Source colors come from the @theme block above:
 *   navy-950 #0F1729 = 220 47% 11%
 *   panel    #252C3D = 222 24% 19%
 *   panel-alt #1a2133 = 223 33% 15%
 *   teal-500 #67B2D7 = 200 60% 62%  (Fabro's "teal" is a sky blue)
 *   mint     #5AC8A8 = 163 49% 57%
 *   ice-100  #E8EDF3 = 213 27% 93%
 *   ice-300  #A8B5C5 = 213 22% 72%
 * ------------------------------------------------------------------------- */
.fabro-chat {
  --aui-background: 220 47% 11%;
  --aui-foreground: 0 0% 100%;
  --aui-card: 222 24% 19%;
  --aui-card-foreground: 0 0% 100%;
  --aui-popover: 222 24% 19%;
  --aui-popover-foreground: 0 0% 100%;
  --aui-primary: 200 60% 62%;
  --aui-primary-foreground: 220 47% 11%;
  --aui-secondary: 223 33% 15%;
  --aui-secondary-foreground: 0 0% 100%;
  --aui-muted: 223 33% 15%;
  --aui-muted-foreground: 213 22% 72%;
  --aui-accent: 163 49% 57%;
  --aui-accent-foreground: 220 47% 11%;
  --aui-destructive: 0 76% 66%;
  --aui-destructive-foreground: 0 0% 100%;
  --aui-border: 218 28% 17%;
  --aui-input: 218 28% 17%;
  --aui-ring: 200 60% 62%;
  --aui-radius: 0.5rem;
  --aui-thread-max-width: 44rem;
}
```

- [ ] **Step 2: Typecheck still clean**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/app.css
git commit -m "style(fabro-web): map assistant-ui --aui-* tokens to Fabro palette"
```

---

### Task 4: Local types module

**Files:**
- Create: `apps/fabro-web/app/lib/chats-types.ts`

- [ ] **Step 1: Create the file**

```ts
import type { CompletionMessage } from "@qltysh/fabro-api-client";

/**
 * Stricter discriminated-union view over @qltysh/fabro-api-client's
 * `CompletionContentPart` ({ kind: string; data: any }). Each variant in our
 * union is assignable to the API client type at the boundary, but inside the
 * chat code we get exhaustive switch checking.
 */
export type ChatContentPart =
  | { kind: "text"; data: { text: string } }
  | {
      kind: "tool_call";
      data: {
        tool_call_id: string;
        name: string;
        arguments: { [key: string]: JsonValue };
      };
    }
  | {
      kind: "tool_result";
      data: {
        tool_call_id: string;
        content: JsonValue;
        is_error?: boolean;
      };
    };

export type JsonValue =
  | null
  | string
  | number
  | boolean
  | JsonValue[]
  | { [key: string]: JsonValue };

/**
 * Sidebar/store wrapper around a single chat. Messages and the in-flight
 * stream live inside assistant-ui's runtime; the store holds the metadata
 * needed to render the sidebar, derive titles, and drive the scripted
 * reply bank. `seedMessages` is the initial history fed to the runtime via
 * `initialMessages` on mount. `pendingResponse` flags a chat where the user
 * sent the first message but the assistant has not yet replied.
 */
export type Chat = {
  id: string;
  title: string;
  createdAt: number;
  scriptIndex: number;
  seedMessages: CompletionMessage[];
  pendingResponse: boolean;
};

export type { CompletionMessage } from "@qltysh/fabro-api-client";
```

- [ ] **Step 2: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/lib/chats-types.ts
git commit -m "feat(fabro-web): chats-types module with Chat wrapper + ChatContentPart"
```

---

### Task 5: Scripted reply bank

**Files:**
- Create: `apps/fabro-web/app/lib/chats-script.ts`

- [ ] **Step 1: Create the file**

Copy the bank verbatim from `docs/superpowers/prototypes/2026-05-16-chats-new/src/lib/chats-script.ts`, but change the import:

```ts
import type { CompletionMessage } from "@qltysh/fabro-api-client";

/**
 * Scripted assistant replies cycled through per chat. Generic content,
 * intentionally not Fabro-specific. Each entry is a single assistant
 * CompletionMessage; tool calls and their results are siblings in the
 * content array so the renderer can pair them.
 */
export const SCRIPTED_REPLIES: CompletionMessage[] = [
  // ... copy all 6 entries from the prototype file verbatim ...
];

export function pickReply(scriptIndex: number): CompletionMessage {
  return SCRIPTED_REPLIES[scriptIndex % SCRIPTED_REPLIES.length]!;
}
```

- [ ] **Step 2: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/lib/chats-script.ts
git commit -m "feat(fabro-web): scripted reply bank for chats prototype"
```

---

### Task 6: Chats store reducer tests (TDD)

**Files:**
- Create: `apps/fabro-web/app/lib/chats-store.test.tsx`

- [ ] **Step 1: Write the failing tests**

```tsx
import { describe, expect, test } from "bun:test";
import { act } from "react-test-renderer";
import { renderHook } from "./test-utils"; // see step 3 below if not yet present

import { ChatsProvider, useChatsStore } from "./chats-store";

function wrapper({ children }: { children: React.ReactNode }) {
  return <ChatsProvider>{children}</ChatsProvider>;
}

describe("chats-store reducer", () => {
  test("createChatWithFirstMessage seeds title and user message", () => {
    const { result } = renderHook(() => useChatsStore(), { wrapper });
    let id = "";
    act(() => {
      id = result.current.createChatWithFirstMessage("Help me with React");
    });
    const chat = result.current.state.chats[id];
    expect(chat?.title).toBe("Help me with React");
    expect(chat?.pendingResponse).toBe(true);
    expect(chat?.seedMessages).toHaveLength(1);
    expect(chat?.seedMessages[0]?.role).toBe("user");
    expect(chat?.seedMessages[0]?.content[0]).toEqual({
      kind: "text",
      data: { text: "Help me with React" },
    });
  });

  test("title is truncated to 40 chars at word boundary", () => {
    const { result } = renderHook(() => useChatsStore(), { wrapper });
    let id = "";
    act(() => {
      id = result.current.createChatWithFirstMessage(
        "Help me draft a launch announcement email for our new analytics dashboard",
      );
    });
    expect(result.current.state.chats[id]?.title).toBe(
      "Help me draft a launch announcement…",
    );
  });

  test("consumePendingResponse clears the flag", () => {
    const { result } = renderHook(() => useChatsStore(), { wrapper });
    let id = "";
    act(() => {
      id = result.current.createChatWithFirstMessage("hi");
    });
    expect(result.current.state.chats[id]?.pendingResponse).toBe(true);
    act(() => {
      result.current.consumePendingResponse(id);
    });
    expect(result.current.state.chats[id]?.pendingResponse).toBe(false);
  });

  test("advanceScriptIndex increments by one", () => {
    const { result } = renderHook(() => useChatsStore(), { wrapper });
    let id = "";
    act(() => {
      id = result.current.createChatWithFirstMessage("hi");
    });
    expect(result.current.state.chats[id]?.scriptIndex).toBe(0);
    act(() => {
      result.current.advanceScriptIndex(id);
    });
    expect(result.current.state.chats[id]?.scriptIndex).toBe(1);
  });

  test("seed chats appear in order on mount", () => {
    const { result } = renderHook(() => useChatsStore(), { wrapper });
    expect(result.current.state.order.length).toBeGreaterThanOrEqual(3);
    const titles = result.current.state.order.map(
      (id) => result.current.state.chats[id]?.title,
    );
    expect(titles).toContain("Draft a launch email");
    expect(titles).toContain("Refactor a React hook");
    expect(titles).toContain("Compare Postgres vs SQLite");
  });
});
```

- [ ] **Step 2: Write a minimal `renderHook` shim if fabro-web doesn't already have one**

Look for an existing `renderHook` in `apps/fabro-web/app/lib/`. If not present, create `apps/fabro-web/app/lib/test-utils.tsx`:

```tsx
import { createElement, type ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";

export function renderHook<T>(
  hook: () => T,
  options: { wrapper: React.ComponentType<{ children: ReactNode }> },
): { result: { current: T } } {
  const result = { current: undefined as unknown as T };
  function HookHost() {
    result.current = hook();
    return null;
  }
  act(() => {
    TestRenderer.create(
      createElement(options.wrapper, null, createElement(HookHost)),
    );
  });
  return { result };
}
```

- [ ] **Step 3: Run the tests — they fail because chats-store does not exist yet**

Run: `cd apps/fabro-web && bun test app/lib/chats-store.test.tsx`
Expected: FAIL with "Cannot find module './chats-store'" (or similar).

- [ ] **Step 4: Commit the failing tests**

```bash
git add apps/fabro-web/app/lib/chats-store.test.tsx apps/fabro-web/app/lib/test-utils.tsx
git commit -m "test(fabro-web): chats-store reducer tests (failing — store not built yet)"
```

---

### Task 7: Implement chats-store

**Files:**
- Create: `apps/fabro-web/app/lib/chats-store.tsx`

- [ ] **Step 1: Write the store**

```tsx
import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useReducer,
  useRef,
  type ReactNode,
} from "react";

import type { Chat, CompletionMessage } from "./chats-types";
import { pickReply } from "./chats-script";

type State = {
  chats: Record<string, Chat>;
  order: string[]; // newest first
};

type Action =
  | {
      type: "create";
      id: string;
      title: string;
      createdAt: number;
      userMessage: CompletionMessage;
    }
  | { type: "consume_pending"; chatId: string }
  | { type: "advance_script"; chatId: string };

function deriveTitle(text: string): string {
  const trimmed = text.trim().replace(/\s+/g, " ");
  if (trimmed.length <= 40) return trimmed || "New chat";
  const cut = trimmed.slice(0, 40);
  const lastSpace = cut.lastIndexOf(" ");
  const base = lastSpace > 20 ? cut.slice(0, lastSpace) : cut;
  return `${base}…`;
}

function userMessage(text: string): CompletionMessage {
  return {
    role: "user",
    content: [{ kind: "text", data: { text } }],
  };
}

function seedChat(
  id: string,
  title: string,
  ageDays: number,
  scriptIndex: number,
  userText: string,
): Chat {
  return {
    id,
    title,
    createdAt: Date.now() - ageDays * 86_400_000,
    scriptIndex: scriptIndex + 1, // seeded reply already "consumed"
    pendingResponse: false,
    seedMessages: [userMessage(userText), pickReply(scriptIndex)],
  };
}

const initialState: State = (() => {
  const seeds: Chat[] = [
    seedChat(
      "seed_email",
      "Draft a launch email",
      0.5,
      0,
      "Help me draft a launch announcement email for our new analytics dashboard.",
    ),
    seedChat(
      "seed_hook",
      "Refactor a React hook",
      2,
      3,
      "My useChat hook has grown to 200 lines and I keep tangling concerns. How should I think about refactoring it?",
    ),
    seedChat(
      "seed_db",
      "Compare Postgres vs SQLite",
      6,
      5,
      "For a side project with ~50 daily users, should I reach for Postgres or stick with SQLite?",
    ),
  ];
  const chats: Record<string, Chat> = {};
  for (const s of seeds) chats[s.id] = s;
  return { chats, order: seeds.map((s) => s.id) };
})();

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "create": {
      const chat: Chat = {
        id: action.id,
        title: action.title,
        createdAt: action.createdAt,
        scriptIndex: 0,
        pendingResponse: true,
        seedMessages: [action.userMessage],
      };
      return {
        chats: { ...state.chats, [action.id]: chat },
        order: [action.id, ...state.order],
      };
    }
    case "consume_pending": {
      const existing = state.chats[action.chatId];
      if (!existing || !existing.pendingResponse) return state;
      return {
        ...state,
        chats: {
          ...state.chats,
          [action.chatId]: { ...existing, pendingResponse: false },
        },
      };
    }
    case "advance_script": {
      const existing = state.chats[action.chatId];
      if (!existing) return state;
      return {
        ...state,
        chats: {
          ...state.chats,
          [action.chatId]: {
            ...existing,
            scriptIndex: existing.scriptIndex + 1,
          },
        },
      };
    }
  }
}

type ChatsContextValue = {
  state: State;
  createChatWithFirstMessage: (text: string) => string;
  consumePendingResponse: (chatId: string) => void;
  peekScriptIndex: (chatId: string) => number;
  advanceScriptIndex: (chatId: string) => void;
};

const ChatsContext = createContext<ChatsContextValue | null>(null);

function shortId(): string {
  return `c_${Math.random().toString(36).slice(2, 8)}`;
}

export function ChatsProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initialState);
  const stateRef = useRef(state);
  stateRef.current = state;

  const createChatWithFirstMessage = useCallback((text: string) => {
    const id = shortId();
    dispatch({
      type: "create",
      id,
      title: deriveTitle(text),
      createdAt: Date.now(),
      userMessage: userMessage(text),
    });
    return id;
  }, []);

  const consumePendingResponse = useCallback((chatId: string) => {
    dispatch({ type: "consume_pending", chatId });
  }, []);

  const peekScriptIndex = useCallback(
    (chatId: string) => stateRef.current.chats[chatId]?.scriptIndex ?? 0,
    [],
  );

  const advanceScriptIndex = useCallback((chatId: string) => {
    dispatch({ type: "advance_script", chatId });
  }, []);

  const value = useMemo<ChatsContextValue>(
    () => ({
      state,
      createChatWithFirstMessage,
      consumePendingResponse,
      peekScriptIndex,
      advanceScriptIndex,
    }),
    [
      state,
      createChatWithFirstMessage,
      consumePendingResponse,
      peekScriptIndex,
      advanceScriptIndex,
    ],
  );

  return <ChatsContext.Provider value={value}>{children}</ChatsContext.Provider>;
}

export function useChatsStore(): ChatsContextValue {
  const value = useContext(ChatsContext);
  if (!value) throw new Error("useChatsStore must be used inside <ChatsProvider>");
  return value;
}
```

- [ ] **Step 2: Run the store tests — they pass**

Run: `cd apps/fabro-web && bun test app/lib/chats-store.test.tsx`
Expected: PASS (5 tests).

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/lib/chats-store.tsx
git commit -m "feat(fabro-web): chats-store with Context + reducer"
```

---

### Task 8: Chats runtime adapter tests (TDD)

**Files:**
- Create: `apps/fabro-web/app/lib/chats-runtime.test.ts`

- [ ] **Step 1: Write the failing tests**

```ts
import { describe, expect, test } from "bun:test";

import { createScriptedAdapter, toThreadMessages } from "./chats-runtime";
import { SCRIPTED_REPLIES } from "./chats-script";
import type { Chat, CompletionMessage } from "./chats-types";

const emptyChat: Chat = {
  id: "c_test",
  title: "",
  createdAt: 0,
  scriptIndex: 0,
  pendingResponse: false,
  seedMessages: [],
};

describe("createScriptedAdapter", () => {
  test("yields chunks ending in the full scripted reply content", async () => {
    let onCompleteCalled = false;
    let completedReply: CompletionMessage | null = null;
    const adapter = createScriptedAdapter({
      getChat: () => ({ ...emptyChat, scriptIndex: 0 }),
      onReplyComplete: (reply) => {
        onCompleteCalled = true;
        completedReply = reply;
      },
    });

    const controller = new AbortController();
    const runResults = [];
    for await (const r of adapter.run({
      messages: [],
      abortSignal: controller.signal,
      runConfig: {},
      context: { tools: [] } as unknown as Parameters<typeof adapter.run>[0]["context"],
      unstable_getMessage: () => ({}) as never,
    })) {
      runResults.push(r);
    }

    expect(onCompleteCalled).toBe(true);
    expect(completedReply).toBe(SCRIPTED_REPLIES[0]);
    // Final result must contain at least one text part with the full text from
    // the first scripted reply.
    const finalContent = runResults[runResults.length - 1]?.content;
    expect(finalContent).toBeDefined();
    const finalText = finalContent
      ?.filter((p) => p.type === "text")
      .map((p) => (p as { type: "text"; text: string }).text)
      .join("");
    const expectedText = SCRIPTED_REPLIES[0]!.content
      .filter((p) => p.kind === "text")
      .map((p) => (p.data as { text: string }).text)
      .join("");
    expect(finalText).toBe(expectedText);
  });

  test("picks reply based on getChat().scriptIndex (wraps modulo bank length)", async () => {
    let completed: CompletionMessage | null = null;
    const adapter = createScriptedAdapter({
      getChat: () => ({ ...emptyChat, scriptIndex: SCRIPTED_REPLIES.length + 2 }),
      onReplyComplete: (reply) => {
        completed = reply;
      },
    });
    const controller = new AbortController();
    for await (const _ of adapter.run({
      messages: [],
      abortSignal: controller.signal,
      runConfig: {},
      context: { tools: [] } as unknown as Parameters<typeof adapter.run>[0]["context"],
      unstable_getMessage: () => ({}) as never,
    })) {
      // drain
    }
    expect(completed).toBe(SCRIPTED_REPLIES[2]);
  });
});

describe("toThreadMessages", () => {
  test("converts a user text message", () => {
    const out = toThreadMessages([
      { role: "user", content: [{ kind: "text", data: { text: "hi" } }] },
    ]);
    expect(out).toEqual([
      { role: "user", content: [{ type: "text", text: "hi" }] },
    ]);
  });

  test("converts an assistant message with paired tool_call + tool_result", () => {
    const out = toThreadMessages([
      {
        role: "assistant",
        content: [
          {
            kind: "tool_call",
            data: {
              tool_call_id: "t1",
              name: "search",
              arguments: { q: "hello" },
            },
          },
          {
            kind: "tool_result",
            data: { tool_call_id: "t1", content: { ok: true } },
          },
        ],
      },
    ]);
    expect(out).toHaveLength(1);
    expect(out[0]?.role).toBe("assistant");
    const parts = out[0]?.content as Array<{
      type: string;
      result?: unknown;
      toolCallId?: string;
    }>;
    expect(parts).toHaveLength(1);
    expect(parts[0]?.type).toBe("tool-call");
    expect(parts[0]?.toolCallId).toBe("t1");
    expect(parts[0]?.result).toEqual({ ok: true });
  });
});
```

- [ ] **Step 2: Run — they fail (no module yet)**

Run: `cd apps/fabro-web && bun test app/lib/chats-runtime.test.ts`
Expected: FAIL with "Cannot find module './chats-runtime'".

- [ ] **Step 3: Commit failing tests**

```bash
git add apps/fabro-web/app/lib/chats-runtime.test.ts
git commit -m "test(fabro-web): chats-runtime adapter tests (failing — module not built)"
```

---

### Task 9: Implement chats-runtime adapter

**Files:**
- Create: `apps/fabro-web/app/lib/chats-runtime.ts`

- [ ] **Step 1: Write the runtime module**

Copy verbatim from `docs/superpowers/prototypes/2026-05-16-chats-new/src/lib/chats-runtime.ts`, but change the import:

```ts
import type {
  ChatModelAdapter,
  ChatModelRunResult,
  ThreadAssistantMessagePart,
  ThreadMessageLike,
} from "@assistant-ui/react";

import type {
  Chat,
  ChatContentPart,
  CompletionMessage,
} from "./chats-types";
import { pickReply } from "./chats-script";

const STREAM_CHUNK_CHARS = 28;
const STREAM_CHUNK_INTERVAL_MS = 55;

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const handle = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(handle);
        reject(new DOMException("Aborted", "AbortError"));
      },
      { once: true },
    );
  });
}

function toAssistantParts(
  content: readonly ChatContentPart[],
): ThreadAssistantMessagePart[] {
  const out: ThreadAssistantMessagePart[] = [];
  for (const part of content) {
    if (part.kind === "text") {
      out.push({ type: "text", text: part.data.text });
    } else if (part.kind === "tool_call") {
      out.push({
        type: "tool-call",
        toolCallId: part.data.tool_call_id,
        toolName: part.data.name,
        args: part.data.arguments,
        argsText: JSON.stringify(part.data.arguments),
      });
    } else if (part.kind === "tool_result") {
      const target = out
        .slice()
        .reverse()
        .find(
          (p): p is Extract<ThreadAssistantMessagePart, { type: "tool-call" }> =>
            p.type === "tool-call" && p.toolCallId === part.data.tool_call_id,
        );
      if (target) {
        const idx = out.lastIndexOf(target);
        out[idx] = { ...target, result: part.data.content };
      }
    }
  }
  return out;
}

export function createScriptedAdapter(args: {
  getChat: () => Chat | undefined;
  onReplyComplete: (reply: CompletionMessage) => void;
}): ChatModelAdapter {
  return {
    async *run({ abortSignal }) {
      const chat = args.getChat();
      const reply = pickReply(chat?.scriptIndex ?? 0);
      const accumulated: ChatContentPart[] = [];

      for (const part of reply.content as ChatContentPart[]) {
        if (part.kind === "text") {
          const text = part.data.text;
          let cursor = 0;
          accumulated.push({ kind: "text", data: { text: "" } });
          const accIndex = accumulated.length - 1;
          while (cursor < text.length) {
            cursor = Math.min(cursor + STREAM_CHUNK_CHARS, text.length);
            accumulated[accIndex] = {
              kind: "text",
              data: { text: text.slice(0, cursor) },
            };
            yield buildUpdate(accumulated);
            if (cursor < text.length) {
              await sleep(STREAM_CHUNK_INTERVAL_MS, abortSignal);
            }
          }
        } else {
          accumulated.push(part);
          yield buildUpdate(accumulated);
          await sleep(STREAM_CHUNK_INTERVAL_MS * 3, abortSignal);
        }
      }

      args.onReplyComplete(reply);
    },
  };
}

function buildUpdate(parts: ChatContentPart[]): ChatModelRunResult {
  return { content: toAssistantParts(parts) };
}

export function toThreadMessages(
  messages: readonly CompletionMessage[],
): ThreadMessageLike[] {
  return messages.map((msg) => {
    if (msg.role === "user") {
      return {
        role: "user",
        content: (msg.content as ChatContentPart[])
          .filter((p): p is Extract<ChatContentPart, { kind: "text" }> =>
            p.kind === "text",
          )
          .map((p) => ({ type: "text", text: p.data.text }) as const),
      };
    }
    if (msg.role === "assistant") {
      return {
        role: "assistant",
        content: toAssistantParts(msg.content as ChatContentPart[]),
      };
    }
    return { role: "system", content: [] };
  });
}
```

- [ ] **Step 2: Run the tests — they pass**

Run: `cd apps/fabro-web && bun test app/lib/chats-runtime.test.ts`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/lib/chats-runtime.ts
git commit -m "feat(fabro-web): scripted ChatModelAdapter + thread-message converter"
```

---

### Task 10: ToolFallback component

**Files:**
- Create: `apps/fabro-web/app/components/chats/tool-fallback.tsx`

- [ ] **Step 1: Create the file**

Copy verbatim from `docs/superpowers/prototypes/2026-05-16-chats-new/src/components/tool-fallback.tsx`. No path changes needed — the imports (`@assistant-ui/react`, `@heroicons/react/24/outline`) are identical and resolve in fabro-web's package.json.

```tsx
import type { ToolCallMessagePartProps } from "@assistant-ui/react";
import { WrenchScrewdriverIcon } from "@heroicons/react/24/outline";

export default function ToolFallback(props: ToolCallMessagePartProps) {
  const { toolName, args, result } = props;
  return (
    <div className="my-2 rounded-lg border border-line bg-overlay/70 text-fg-2">
      <div className="flex items-center gap-2 border-b border-line px-3 py-2 text-xs font-medium uppercase tracking-wide text-fg-muted">
        <WrenchScrewdriverIcon className="size-3.5" />
        <span>tool</span>
        <code className="rounded bg-overlay-strong px-1.5 py-0.5 font-mono text-[11px] normal-case tracking-normal text-fg">
          {toolName}
        </code>
      </div>
      <Section label="arguments">
        <pre className="whitespace-pre-wrap break-words font-mono text-[12px] leading-relaxed text-fg-2">
          {formatJson(args)}
        </pre>
      </Section>
      {result !== undefined && (
        <Section label="result">
          <pre className="whitespace-pre-wrap break-words font-mono text-[12px] leading-relaxed text-fg-2">
            {formatJson(result)}
          </pre>
        </Section>
      )}
    </div>
  );
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="border-b border-line last:border-b-0">
      <div className="px-3 pt-2 text-[10px] font-medium uppercase tracking-wider text-fg-muted">
        {label}
      </div>
      <div className="px-3 pb-2">{children}</div>
    </div>
  );
}

function formatJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
```

- [ ] **Step 2: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/components/chats/tool-fallback.tsx
git commit -m "feat(fabro-web): ToolFallback renderer for tool-call message parts"
```

---

### Task 11: Composer chips component

**Files:**
- Create: `apps/fabro-web/app/components/chats/composer-chips.tsx`

- [ ] **Step 1: Create the file**

Copy verbatim from `docs/superpowers/prototypes/2026-05-16-chats-new/src/components/composer-chips.tsx`. No import path changes — all paths are package-relative.

(See prototype file for full content; ~80 lines.)

- [ ] **Step 2: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/components/chats/composer-chips.tsx
git commit -m "feat(fabro-web): decorative project/branch/model chips for the chat composer"
```

---

### Task 12: Custom composer component

**Files:**
- Create: `apps/fabro-web/app/components/chats/custom-composer.tsx`

- [ ] **Step 1: Create the file**

Copy verbatim from `docs/superpowers/prototypes/2026-05-16-chats-new/src/components/custom-composer.tsx`. Adjust the chips import path:

```tsx
import ComposerChips from "./composer-chips";
```

is already relative, so the verbatim copy works.

(See prototype file for full content; ~45 lines.)

- [ ] **Step 2: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/components/chats/custom-composer.tsx
git commit -m "feat(fabro-web): custom composer wrapping ComposerPrimitive + chips"
```

---

### Task 13: chats-layout route

**Files:**
- Create: `apps/fabro-web/app/routes/chats-layout.tsx`

- [ ] **Step 1: Create the file**

Copy verbatim from `docs/superpowers/prototypes/2026-05-16-chats-new/src/routes/chats-layout.tsx`, then:

1. Change the store import to `from "../lib/chats-store"`.
2. Add a route handle at the top of the file:

```tsx
export const handle = { fullHeight: true, wide: true };
```

The layout itself stays unchanged from the prototype (sidebar with translucent `bg-panel/40`, active-item teal accent, `relative isolate` for stacking, `Outlet` for the active route).

- [ ] **Step 2: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/routes/chats-layout.tsx
git commit -m "feat(fabro-web): chats-layout route (sidebar + outlet, fullHeight handle)"
```

---

### Task 14: chats-new route

**Files:**
- Create: `apps/fabro-web/app/routes/chats-new.tsx`

- [ ] **Step 1: Create the file**

Copy from `docs/superpowers/prototypes/2026-05-16-chats-new/src/routes/chats-new.tsx`, then change the submit handler to use the new store API (no router state — chat-store carries pending message). The `handle` for AppShell layout is set on the parent (chats-layout), not on this child route.

```tsx
import { useRef, useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { ArrowUpIcon } from "@heroicons/react/24/solid";

import { useChatsStore } from "../lib/chats-store";
import ComposerChips from "../components/chats/composer-chips";

export function meta() {
  return [{ title: "New chat — Fabro" }];
}

export default function ChatsNew() {
  const navigate = useNavigate();
  const { createChatWithFirstMessage } = useChatsStore();
  const [text, setText] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  function submit(value: string) {
    const trimmed = value.trim();
    if (!trimmed) return;
    const id = createChatWithFirstMessage(trimmed);
    navigate(`/chats/${id}`);
  }

  function onSubmit(e: FormEvent) {
    e.preventDefault();
    submit(text);
  }

  return (
    <div className="flex h-full flex-col items-center px-6 pt-[18vh] pb-10">
      <div className="w-full max-w-2xl">
        <form
          onSubmit={onSubmit}
          className="w-full overflow-hidden rounded-2xl bg-panel-alt/80 shadow-2xl shadow-black/40 ring-1 ring-line-strong backdrop-blur-sm transition-all focus-within:ring-teal-500/40"
        >
          <textarea
            ref={textareaRef}
            name="prompt"
            aria-label="Message"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit(text);
              }
            }}
            placeholder="Ask anything…"
            rows={2}
            autoFocus
            className="block max-h-72 w-full resize-none bg-transparent px-4 pt-4 pb-2 text-base text-fg placeholder:text-fg-muted focus:outline-none"
          />
          <div className="flex items-center justify-between gap-3 px-4 pt-2 pb-3">
            <ComposerChips />
            <button
              type="submit"
              disabled={!text.trim()}
              aria-label="Send message"
              className="inline-flex size-9 items-center justify-center rounded-full bg-teal-500 text-on-primary transition-colors hover:bg-teal-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-teal-500"
            >
              <ArrowUpIcon className="size-4" />
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/routes/chats-new.tsx
git commit -m "feat(fabro-web): chats-new empty-state route"
```

---

### Task 15: chats-detail route

**Files:**
- Create: `apps/fabro-web/app/routes/chats-detail.tsx`

- [ ] **Step 1: Create the file**

```tsx
import { useEffect, useMemo, useRef } from "react";
import { useNavigate, useParams } from "react-router";
import {
  AssistantRuntimeProvider,
  useLocalRuntime,
} from "@assistant-ui/react";
import { Thread, makeMarkdownText } from "@assistant-ui/react-ui";

import { useChatsStore } from "../lib/chats-store";
import {
  createScriptedAdapter,
  toThreadMessages,
} from "../lib/chats-runtime";
import CustomComposer from "../components/chats/custom-composer";
import ToolFallback from "../components/chats/tool-fallback";
import type { CompletionMessage } from "../lib/chats-types";

// AppShell handle lives on the parent chats-layout route; do not redeclare it
// here.

const MarkdownText = makeMarkdownText();

export default function ChatsDetail() {
  const { chatId } = useParams<{ chatId: string }>();
  const navigate = useNavigate();
  const { state } = useChatsStore();
  const chat = chatId ? state.chats[chatId] : undefined;

  if (!chatId || !chat) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-center">
          <p className="text-sm text-fg-muted">That chat doesn&rsquo;t exist.</p>
          <button
            type="button"
            onClick={() => navigate("/chats/new")}
            className="mt-3 text-sm font-medium text-teal-300 hover:text-teal-500"
          >
            Start a new chat
          </button>
        </div>
      </div>
    );
  }

  return <ChatRuntime key={chatId} chatId={chatId} />;
}

function ChatRuntime({ chatId }: { chatId: string }) {
  const {
    state,
    peekScriptIndex,
    advanceScriptIndex,
    consumePendingResponse,
  } = useChatsStore();
  const chat = state.chats[chatId]!;

  const initialMessages = useMemo(
    () => toThreadMessages(chat.seedMessages),
    [chat.seedMessages],
  );

  const adapter = useMemo(
    () =>
      createScriptedAdapter({
        getChat: () => ({
          ...chat,
          scriptIndex: peekScriptIndex(chatId),
        }),
        onReplyComplete: (_reply: CompletionMessage) =>
          advanceScriptIndex(chatId),
      }),
    [chat, chatId, peekScriptIndex, advanceScriptIndex],
  );

  const runtime = useLocalRuntime(adapter, { initialMessages });

  // Autorespond: chats arriving here from /chats/new carry the user's first
  // message in seedMessages with pendingResponse=true. Trigger one startRun
  // and immediately mark the pending flag consumed so the next render is a
  // no-op. Safe under StrictMode because startRun is idempotent on a thread
  // whose last message is a user message — the store-level flag dedupes.
  const didStartRef = useRef(false);
  useEffect(() => {
    if (!chat.pendingResponse || didStartRef.current) return;
    didStartRef.current = true;
    consumePendingResponse(chatId);
    runtime.thread.startRun({ parentId: null });
  }, [chat.pendingResponse, chatId, consumePendingResponse, runtime]);

  return (
    <AssistantRuntimeProvider runtime={runtime}>
      <div className="h-full">
        <Thread
          components={{ Composer: CustomComposer }}
          assistantMessage={{
            components: { Text: MarkdownText, ToolFallback },
          }}
        />
      </div>
    </AssistantRuntimeProvider>
  );
}
```

- [ ] **Step 2: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add apps/fabro-web/app/routes/chats-detail.tsx
git commit -m "feat(fabro-web): chats-detail with assistant-ui Thread + scripted runtime"
```

---

### Task 16: Wire chats routes into router.tsx

**Files:**
- Modify: `apps/fabro-web/app/router.tsx`

- [ ] **Step 1: Add the imports**

Open `apps/fabro-web/app/router.tsx`. Add to the import block alongside the existing `import * as X from "./routes/x"` lines:

```ts
import * as ChatsLayout from "./routes/chats-layout";
import * as ChatsNew from "./routes/chats-new";
import * as ChatsDetail from "./routes/chats-detail";
```

- [ ] **Step 2: Register the routes inside the AppShell tree**

Find the route object whose `Component: withRouteModule({ default: AppShellModule })` wraps the in-app routes (`start`, `automations`, `runs`, etc.). Add this entry to its `children` array, placed near the top so it appears prominently in the router definition:

```ts
route("chats", ChatsLayout, {
  children: [
    route("new", ChatsNew),
    route(":chatId", ChatsDetail),
  ],
}),
```

No index route — `/chats` with no suffix is intentionally not a destination. `/chats/new` is the canonical empty-state URL; the sidebar's "New chat" button navigates there explicitly.

- [ ] **Step 3: Typecheck**

Run: `cd apps/fabro-web && bun run typecheck`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add apps/fabro-web/app/router.tsx
git commit -m "feat(fabro-web): wire chats routes under AppShell"
```

---

### Task 17: Router resolution test

**Files:**
- Create: `apps/fabro-web/app/routes/chats-router.test.tsx`

- [ ] **Step 1: Write the test**

```tsx
import { describe, expect, test } from "bun:test";
import { MemoryRouter, Routes, Route } from "react-router";
import TestRenderer, { act } from "react-test-renderer";

import * as ChatsLayoutModule from "./chats-layout";
import * as ChatsNewModule from "./chats-new";
import * as ChatsDetailModule from "./chats-detail";

describe("chats route module exports", () => {
  test("each route exports a default component", () => {
    expect(typeof ChatsLayoutModule.default).toBe("function");
    expect(typeof ChatsNewModule.default).toBe("function");
    expect(typeof ChatsDetailModule.default).toBe("function");
  });

  test("chats-layout declares the AppShell handle (children inherit via useMatches)", () => {
    expect(ChatsLayoutModule.handle).toEqual({ fullHeight: true, wide: true });
  });

  test("chats-new renders inside MemoryRouter without crashing", () => {
    let tree: TestRenderer.ReactTestRenderer | null = null;
    act(() => {
      tree = TestRenderer.create(
        <MemoryRouter initialEntries={["/"]}>
          <Routes>
            <Route path="/" Component={ChatsNewModule.default} />
          </Routes>
        </MemoryRouter>,
      );
    });
    expect(tree).not.toBeNull();
    act(() => {
      tree?.unmount();
    });
  });
});
```

- [ ] **Step 2: Run — expect green**

Run: `cd apps/fabro-web && bun test app/routes/chats-router.test.tsx`
Expected: PASS (3 tests).

- [ ] **Step 3: Run the full test suite to verify nothing regressed**

Run: `cd apps/fabro-web && bun test`
Expected: PASS overall.

- [ ] **Step 4: Commit**

```bash
git add apps/fabro-web/app/routes/chats-router.test.tsx
git commit -m "test(fabro-web): chats route module exports + handles"
```

---

### Task 18: Manual browser smoke test

**Files:** none.

This is a manual verification step. UI-correctness is what the spec calls out:
*"For UI or frontend changes, start the dev server and use the feature in a browser before reporting the task as complete."* (CLAUDE.md global rule.)

- [ ] **Step 1: Start the dev servers**

```bash
fabro server start
```

In a second terminal:

```bash
cd apps/fabro-web && bun run dev
```

- [ ] **Step 2: Walk through the golden path**

Open the app in a browser. Sign in / be on the in-app shell. Navigate to `/chats/new`.

Verify:
- [ ] Empty state renders: centered composer with chips below textarea.
- [ ] Fabro top nav stays visible above the chat surface.
- [ ] Sidebar shows three seeded chats: "Draft a launch email", "Refactor a React hook", "Compare Postgres vs SQLite".
- [ ] Sidebar's "New chat" button takes you back to `/chats/new`.
- [ ] Typing a message and hitting Enter navigates to `/chats/{id}`, the user message renders right-aligned, and a scripted reply streams in below it.
- [ ] First reply is the greeting + bullet list (with bold/italics/code rendered as markdown, not raw text).
- [ ] Sending again advances the script; the second reply shows a fenced TypeScript code block with the language label `ts` and a copy button.
- [ ] Sending again triggers a reply with a tool-call card (TOOL header, ARGUMENTS section, RESULT section).
- [ ] Clicking a seeded chat in the sidebar swaps the conversation; history is preserved.
- [ ] Composer chips (project / branch / model) open Listbox popovers; selecting an option updates the chip label.
- [ ] Reloading `/chats/{id}` for a freshly created chat keeps the conversation visible (this exercises `seedMessages` + StrictMode autorespond — no empty assistant placeholder).
- [ ] Reloading `/chats/{id}` for an unknown id renders "That chat doesn't exist" with a "Start a new chat" link.

- [ ] **Step 3: Check the console for errors**

Open DevTools. Confirm no red errors during the walkthrough (warnings about missing route components or runtime are red flags).

- [ ] **Step 4: Verify the StrictMode fix specifically**

This is the regression we are guarding against. With `<StrictMode>` still in `apps/fabro-web/app/entry.tsx`:
- Open `/chats/new`.
- Type "Hello world" and submit.
- Confirm the assistant reply streams in completely — not an empty placeholder with just copy/refresh icons.
- Refresh the page on `/chats/{id}` (the page reload remounts the runtime). Conversation should render from `seedMessages`; no additional assistant run should fire (the `pendingResponse` flag was already consumed).

- [ ] **Step 5: If anything failed in Step 2 or 4, do not declare done**

Document the failure, find a fix, add a regression test where possible, and rerun. Do not commit a "good enough" workaround.

- [ ] **Step 6: Commit (docs only — note completion)**

Add a one-line entry to the spec doc's "Open items deferred to follow-up work" section moving the just-completed work out of the list:

```bash
# Edit docs/superpowers/specs/2026-05-16-chats-new-prototype-design.md
# Strike or remove "Wiring the composer chips to actual project/branch/model state, and to a real agent runner."
# from the bottom list since that lives in the next phase.
git add docs/superpowers/specs/2026-05-16-chats-new-prototype-design.md
git commit -m "docs: mark chats-new + chats-detail port complete"
```

---

## Self-review

This plan covers each section of the spec:

- **Route placement** (spec §"Route and layout"): Tasks 13 + 16. Routes mount inside AppShell with `handle: { fullHeight: true, wide: true }`.
- **Files** (spec §"Files"): Tasks 4, 5, 7, 9, 10, 11, 12, 13, 14, 15. All listed paths covered.
- **Visual styling** (spec §"Visual styling"): Tasks 2 and 3. The `@layer` fix and `--aui-*` theme overrides are explicitly addressed.
- **Composer chips** (spec §"Composer chips"): Task 11.
- **Data model** (spec §"Data model"): Tasks 4 and 7. `Chat` wrapper introduced; reuses `CompletionMessage` from the API client.
- **assistant-ui adapter boundary** (spec §"adapter boundary"): Task 9.
- **Scripted replies** (spec §"Scripted replies"): Task 5.
- **Seed data** (spec §"Seed data"): Task 7 (in `initialState`).
- **Title derivation** (spec §"Title derivation"): Task 7 (`deriveTitle`).
- **Behavior** (spec §"Behavior"): Tasks 14, 15, 18. Includes the StrictMode-safe handoff replacement.
- **Error handling** (spec §"Error handling"): Task 15 (fallback empty-state for unknown chatId).
- **Testing** (spec §"Testing"): Tasks 6, 8, 17. Reducer, adapter, route resolution — three focused tests as called out.
- **Verification before declaring done** (spec §"Verification before declaring done"): Task 18.

**Tailwind v4 + cascade-layer ordering** is mandatory per the spec; covered in Task 2.

**ToolFallback + markdown opt-in** are mandatory per the spec; both wired in Task 15.

**StrictMode-safe autorespond** is the deviation from the prototype the plan must address; covered in the architectural note and implemented in Tasks 7 (`pendingResponse` flag, `consumePendingResponse`) and 15 (`startRun({ parentId: null })` + `didStartRef` + flag consume).

No placeholders, TBDs, or "implement later" sections. Each step has either exact file content or exact commands with expected output.
