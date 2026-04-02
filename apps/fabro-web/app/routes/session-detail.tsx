import { useState } from "react";
import { Link, useParams } from "react-router";
import {
  ChatBubbleLeftIcon,
  ClipboardDocumentIcon,
  CheckIcon,
  PencilSquareIcon,
  UserIcon,
} from "@heroicons/react/24/outline";
import { ToolRow, ToolBlock } from "../components/tool-use";
import type { ToolUse } from "../components/tool-use";
import { timeAgo, groupSessionsByDate } from "../lib/time";
import { apiJson } from "../api";
import type { SessionDetail as ApiSessionDetail, PaginatedSessionList } from "@qltysh/fabro-api-client";

export const handle = { hideHeader: true, wide: true };

export function meta({}: any) {
  return [{ title: "Session — Fabro" }];
}

export async function loader({ request, params }: any) {
  const [apiSession, { data: apiSessions }] = await Promise.all([
    apiJson<ApiSessionDetail>(`/sessions/${params.sessionId}`, { request }),
    apiJson<PaginatedSessionList>("/sessions", { request }),
  ]);
  const session: Session = {
    id: apiSession.id,
    title: apiSession.title,
    model: apiSession.model.id,
    created_at: apiSession.created_at,
    updated_at: apiSession.updated_at,
    turns: apiSession.turns.map((t): Turn => {
      switch (t.kind) {
        case "tool":
          return {
            kind: "tool",
            tools: t.tools.map((tu) => ({
              id: tu.id,
              toolName: tu.tool_name,
              input: tu.input,
              result: tu.result,
              isError: tu.is_error,
              durationMs: tu.duration_ms,
            })),
          };
        case "user":
          return { kind: "user", content: t.content, created_at: t.created_at };
        case "assistant":
          return { kind: "assistant", content: t.content };
      }
    }),
  };
  const sessionGroups = groupSessionsByDate(
    apiSessions.map((s) => ({ id: s.id, title: s.title, created_at: s.created_at }))
  );
  return { session, sessionGroups };
}

type Turn =
  | { kind: "user"; content: string; created_at?: string }
  | { kind: "assistant"; content: string }
  | { kind: "tool"; tools: ToolUse[] };

interface Session {
  id: string;
  title: string;
  model: string;
  created_at: string;
  updated_at: string;
  turns: Turn[];
}

// Keep hardcoded sessions as fallback
const sessions: Record<string, Session> = {
  s1: {
    id: "s1",
    title: "Add rate limiting to auth endpoints",
    model: "Opus 4.6",
    created_at: "2026-03-06T14:30:00Z",
    updated_at: "2026-03-06T15:45:00Z",
    turns: [
      {
        kind: "user",
        created_at: "2026-02-28T10:00:00Z",
        content: "Add rate limiting to the auth endpoints. We're getting hit with brute force attempts on /api/auth/login and /api/auth/register. Use a sliding window approach with Redis, 10 requests per minute per IP.",
      },
      {
        kind: "assistant",
        content: "I'll implement sliding window rate limiting using Redis. Let me first look at the existing auth routes and middleware setup.",
      },
      {
        kind: "tool",
        tools: [
          {
            id: "toolu_s1_01",
            toolName: "read_file",
            input: `{ "path": "src/routes/auth.ts" }`,
            result: `import { Router } from "express";\nimport { loginHandler } from "../handlers/auth";\nimport { registerHandler } from "../handlers/auth";\n\nconst router = Router();\nrouter.post("/login", loginHandler);\nrouter.post("/register", registerHandler);\n\nexport default router;`,
            isError: false,
            durationMs: 45,
          },
          {
            id: "toolu_s1_02",
            toolName: "read_file",
            input: `{ "path": "src/middleware/index.ts" }`,
            result: `export { authMiddleware } from "./auth";\nexport { corsMiddleware } from "./cors";\nexport { loggingMiddleware } from "./logging";`,
            isError: false,
            durationMs: 38,
          },
          {
            id: "toolu_s1_03",
            toolName: "search_files",
            input: `{ "pattern": "redis", "path": "src/" }`,
            result: `src/lib/redis.ts:1: import Redis from "ioredis";\nsrc/lib/redis.ts:3: export const redis = new Redis(process.env.REDIS_URL);`,
            isError: false,
            durationMs: 210,
          },
        ],
      },
      {
        kind: "assistant",
        content: "Good — there's already a Redis client at `src/lib/redis.ts`. I'll create a rate limiting middleware using the sliding window algorithm and apply it to the auth routes.\n\nHere's the plan:\n1. Create `src/middleware/rate-limit.ts` with sliding window logic\n2. Apply it to `/login` and `/register` routes\n3. Return `429 Too Many Requests` when the limit is exceeded",
      },
      {
        kind: "tool",
        tools: [
          {
            id: "toolu_s1_04",
            toolName: "write_file",
            input: `{ "path": "src/middleware/rate-limit.ts" }`,
            result: `File written: src/middleware/rate-limit.ts (47 lines)`,
            isError: false,
            durationMs: 65,
          },
          {
            id: "toolu_s1_05",
            toolName: "edit_file",
            input: `{ "path": "src/routes/auth.ts", "operations": [{ "type": "insert", "line": 3 }] }`,
            result: `Applied 1 edit to src/routes/auth.ts`,
            isError: false,
            durationMs: 42,
          },
          {
            id: "toolu_s1_06",
            toolName: "edit_file",
            input: `{ "path": "src/middleware/index.ts", "operations": [{ "type": "append" }] }`,
            result: `Applied 1 edit to src/middleware/index.ts`,
            isError: false,
            durationMs: 55,
          },
        ],
      },
      {
        kind: "assistant",
        content: "Done. I've created the rate limiter and wired it up:\n\n- **`src/middleware/rate-limit.ts`** — Sliding window rate limiter using Redis sorted sets. Each request adds a timestamped entry, expired entries are pruned, and the count is checked against the limit.\n- **`src/routes/auth.ts`** — Applied `rateLimit({ windowMs: 60_000, max: 10 })` to both `/login` and `/register`.\n- **`src/middleware/index.ts`** — Re-exported the new middleware.\n\nWhen the limit is exceeded, clients get a `429` response with a `Retry-After` header indicating seconds until the window resets.",
      },
    ],
  },
  s2: {
    id: "s2",
    title: "Fix config parsing for nested values",
    model: "Sonnet 4.6",
    created_at: "2026-03-06T12:30:00Z",
    updated_at: "2026-03-06T13:15:00Z",
    turns: [
      {
        kind: "user",
        created_at: "2026-02-28T10:00:00Z",
        content: "The CLI crashes when parsing nested TOML config values like [database.connection]. Can you debug and fix this?",
      },
      {
        kind: "assistant",
        content: "Let me look at the config parser to understand how nested keys are handled.",
      },
      {
        kind: "tool",
        tools: [
          {
            id: "toolu_s2_01",
            toolName: "read_file",
            input: `{ "path": "src/config/parser.ts" }`,
            result: `export function parseConfig(raw: string): Record<string, string> {\n  const result: Record<string, string> = {};\n  for (const line of raw.split("\\n")) {\n    const match = line.match(/^(\\w+)\\s*=\\s*(.+)$/);\n    if (match) result[match[1]] = match[2].trim();\n  }\n  return result;\n}`,
            isError: false,
            durationMs: 52,
          },
        ],
      },
      {
        kind: "assistant",
        content: "Found the issue. The parser uses a flat `Record<string, string>` and the regex `^(\\w+)` only matches simple keys — it can't handle TOML section headers like `[database.connection]` or dotted keys like `database.host`.\n\nI'll rewrite the parser to support nested sections and dotted keys, producing a properly nested object.",
      },
      {
        kind: "tool",
        tools: [
          {
            id: "toolu_s2_02",
            toolName: "write_file",
            input: `{ "path": "src/config/parser.ts" }`,
            result: `File written: src/config/parser.ts (62 lines)`,
            isError: false,
            durationMs: 78,
          },
          {
            id: "toolu_s2_03",
            toolName: "run_command",
            input: `{ "command": "npm test -- --grep 'config parser'" }`,
            result: `PASS src/config/__tests__/parser.test.ts\n  config parser\n    ✓ parses flat keys (2ms)\n    ✓ parses section headers (1ms)\n    ✓ parses dotted keys (1ms)\n    ✓ handles nested sections (1ms)\n\nTest Suites: 1 passed\nTests: 4 passed`,
            isError: false,
            durationMs: 2150,
          },
        ],
      },
      {
        kind: "assistant",
        content: "Fixed. The parser now:\n\n1. Tracks the current section header (e.g. `[database.connection]`)\n2. Constructs nested objects from dotted paths\n3. Handles both `key = value` within sections and bare `dotted.key = value` syntax\n\nAll 4 tests pass, including the new nested section case.",
      },
    ],
  },
  s3: {
    id: "s3",
    title: "Migrate to React Router v7",
    model: "Opus 4.6",
    created_at: "2026-03-05T10:00:00Z",
    updated_at: "2026-03-05T11:30:00Z",
    turns: [
      { kind: "user", created_at: "2026-02-26T10:00:00Z", content: "Help me migrate our app from React Router v6 to v7. We're using createBrowserRouter with data loaders." },
      { kind: "assistant", content: "I'll audit your current router setup and identify what needs to change for v7. Let me scan the codebase." },
      {
        kind: "tool",
        tools: [
          { id: "toolu_s3_01", toolName: "search_files", input: `{ "pattern": "createBrowserRouter" }`, result: `src/main.tsx:5: import { createBrowserRouter } from "react-router-dom";\nsrc/main.tsx:8: const router = createBrowserRouter([`, isError: false, durationMs: 180 },
          { id: "toolu_s3_02", toolName: "read_file", input: `{ "path": "package.json" }`, result: `"react-router-dom": "^6.22.0"`, isError: false, durationMs: 35 },
        ],
      },
      { kind: "assistant", content: "You're on React Router 6.22. The migration to v7 involves:\n\n1. Replace `react-router-dom` with `react-router` (single package)\n2. Update `createBrowserRouter` import path\n3. Migrate data loaders to the new `any` type\n4. Update any `useLoaderData` calls to be type-safe\n\nLet me make these changes." },
    ],
  },
};

// Fallback for unknown session IDs
function makeFallbackSession(id: string): Session {
  return {
    id,
    title: "Session",
    model: "Opus 4.6",
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
    turns: [
      { kind: "user", created_at: "2026-02-28T10:00:00Z", content: "Hello, let's get started." },
      { kind: "assistant", content: "Sure! What would you like to work on?" },
    ],
  };
}

interface SessionGroupType {
  label: string;
  sessions: { id: string; title: string; created_at: string }[];
}

const sessionGroups: SessionGroupType[] = [
  {
    label: "Today",
    sessions: [
      { id: "s1", title: "Add rate limiting to auth endpoints", created_at: "2026-03-06T14:30:00Z" },
      { id: "s2", title: "Fix config parsing for nested values", created_at: "2026-03-06T12:30:00Z" },
    ],
  },
  {
    label: "Yesterday",
    sessions: [
      { id: "s3", title: "Migrate to React Router v7", created_at: "2026-03-05T10:00:00Z" },
      { id: "s4", title: "Add dark mode toggle", created_at: "2026-03-05T09:00:00Z" },
      { id: "s5", title: "Update OpenAPI spec for v3", created_at: "2026-03-05T08:00:00Z" },
    ],
  },
  {
    label: "Previous 7 days",
    sessions: [
      { id: "s6", title: "Terraform module for Redis cluster", created_at: "2026-03-03T15:00:00Z" },
      { id: "s7", title: "Add pipeline event types", created_at: "2026-03-01T11:00:00Z" },
      { id: "s8", title: "Implement webhook retry logic", created_at: "2026-02-28T09:00:00Z" },
    ],
  },
];

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  function handleCopy() {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }

  return (
    <button
      type="button"
      onClick={handleCopy}
      className="inline-flex items-center justify-center rounded-md border border-line bg-panel/80 p-1.5 text-fg-muted transition-colors hover:border-line-strong hover:text-fg-3"
      aria-label="Copy"
    >
      {copied
        ? <CheckIcon className="size-3.5" />
        : <ClipboardDocumentIcon className="size-3.5" />}
    </button>
  );
}

function UserBlock({ content, created_at }: { content: string; created_at?: string }) {
  return (
    <div className="group">
      <div className="flex gap-3">
        <div className="flex size-7 shrink-0 items-center justify-center rounded-full bg-panel border border-line-strong">
          <UserIcon className="size-3.5 text-fg-3" />
        </div>
        <div className="min-w-0 flex-1 pt-0.5">
          <pre className="whitespace-pre-wrap font-sans text-sm leading-relaxed text-fg-2">{content}</pre>
        </div>
      </div>
      <div className="ml-10 mt-1 flex h-6 items-center gap-2 opacity-0 transition-opacity group-hover:opacity-100">
        <CopyButton text={content} />
        {created_at != null && <span className="text-[11px] text-fg-muted">{timeAgo(created_at)}</span>}
      </div>
    </div>
  );
}

function AssistantBlock({ content, showCopy }: { content: string; showCopy: boolean }) {
  return (
    <div className="group">
      <div className="flex gap-3">
        <div className="flex size-7 shrink-0 items-center justify-center rounded-full bg-teal-500/10 border border-teal-500/20">
          <ChatBubbleLeftIcon className="size-3.5 text-teal-500" />
        </div>
        <div className="min-w-0 flex-1 pt-0.5">
          <pre className="whitespace-pre-wrap font-sans text-sm leading-relaxed text-fg-3">{content}</pre>
        </div>
      </div>
      {showCopy && (
        <div className="ml-10 mt-1 h-6 opacity-0 transition-opacity group-hover:opacity-100">
          <CopyButton text={content} />
        </div>
      )}
    </div>
  );
}

function SessionSidebar({ activeId, groups }: { activeId: string; groups: SessionGroupType[] }) {
  return (
    <aside className="w-64 shrink-0 border-r border-line flex flex-col h-[calc(100vh-4rem)]">
      <div className="p-3">
        <Link
          to="/start"
          className="flex w-full items-center gap-2 rounded-lg border border-line bg-panel/60 px-3 py-2 text-sm text-fg-2 transition-colors hover:bg-panel hover:border-line-strong"
        >
          <PencilSquareIcon className="size-4 text-fg-muted" />
          New session
        </Link>
      </div>
      <nav className="flex-1 overflow-y-auto px-3 pb-4">
        {groups.map((group) => (
          <div key={group.label} className="mt-4 first:mt-1">
            <p className="px-2 mb-1.5 text-[11px] font-medium uppercase tracking-wider text-fg-muted">
              {group.label}
            </p>
            <ul className="space-y-0.5">
              {group.sessions.map((s) => (
                <li key={s.id}>
                  <Link
                    to={`/sessions/${s.id}`}
                    className={`flex w-full flex-col rounded-lg px-2.5 py-2 text-left transition-colors ${
                      activeId === s.id
                        ? "bg-overlay text-fg-2"
                        : "text-fg-3 hover:bg-overlay"
                    }`}
                  >
                    <span className="truncate text-sm">{s.title}</span>
                    <span className="flex items-center gap-1.5 mt-0.5">
                      <span className="text-[11px] text-fg-muted">{timeAgo(s.created_at)}</span>
                    </span>
                  </Link>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </nav>
    </aside>
  );
}

export default function SessionDetail({ loaderData }: any) {
  const { session, sessionGroups: loaderGroups } = loaderData;

  return (
    <div className="flex -mx-4 sm:-mx-6 lg:-mx-8 -my-6">
      <SessionSidebar activeId={session.id} groups={loaderGroups} />

      <div className="flex-1 flex flex-col min-h-[calc(100vh-4rem)]">
        <div className="border-b border-line px-6 py-3 flex items-center gap-3">
          <h1 className="text-sm font-medium text-fg-2">{session.title}</h1>
          <span className="text-xs text-fg-muted">{timeAgo(session.created_at)}</span>
          <span className="ml-auto font-mono text-xs text-fg-muted">{session.model}</span>
        </div>

        <div className="flex-1 overflow-y-auto px-6 py-6">
          <div className="mx-auto max-w-3xl space-y-5">
            {session.turns.map((turn, i) => {
              switch (turn.kind) {
                case "user":
                  return <UserBlock key={`turn-${i}`} content={turn.content} created_at={turn.created_at} />;
                case "assistant": {
                  const next = session.turns[i + 1];
                  const showCopy = next?.kind !== "tool";
                  return <AssistantBlock key={`turn-${i}`} content={turn.content} showCopy={showCopy} />;
                }
                case "tool":
                  return <div key={`turn-${i}`} className="pl-10"><ToolBlock tools={turn.tools} /></div>;
              }
            })}
          </div>
        </div>

        <div className="border-t border-line px-6 py-4">
          <div className="mx-auto max-w-3xl">
            <div className="flex items-start gap-3 rounded-lg border border-line bg-panel/80 px-4 py-3 focus-within:border-focus">
              <textarea
                placeholder="Send a message..."
                rows={1}
                className="flex-1 resize-none bg-transparent text-sm text-fg-2 placeholder-fg-muted outline-none"
              />
              <button
                type="button"
                className="flex size-8 shrink-0 items-center justify-center rounded-md bg-teal-500 text-white transition-colors hover:bg-teal-400"
              >
                <svg viewBox="0 0 20 20" fill="currentColor" className="size-4" aria-hidden="true">
                  <path d="M3.105 2.288a.75.75 0 0 0-.826.95l1.414 4.926A1.5 1.5 0 0 0 5.135 9.25h6.115a.75.75 0 0 1 0 1.5H5.135a1.5 1.5 0 0 0-1.442 1.086l-1.414 4.926a.75.75 0 0 0 .826.95l14.095-5.637a.75.75 0 0 0 0-1.395L3.105 2.289Z" />
                </svg>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
