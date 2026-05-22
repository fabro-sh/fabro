import type {
  ChatModelAdapter,
  ChatModelRunResult,
  ThreadAssistantMessagePart,
} from "@assistant-ui/react";

import {
  attachSessionEvents,
  streamSessionTurn,
  type SessionStreamEvent,
} from "./session-stream";
import { sessionsApi } from "./api-client";

const SESSION_STORAGE_PREFIX = "fabro:ask-fabro-session:";

function sessionStorageKey(runId: string): string {
  return `${SESSION_STORAGE_PREFIX}${runId}`;
}

interface PersistedSessionState {
  read(runId: string): string | null;
  write(runId: string, sessionId: string): void;
  clear(runId: string): void;
}

const defaultPersistedSessionState: PersistedSessionState = {
  read(runId) {
    if (typeof sessionStorage === "undefined") return null;
    try {
      return sessionStorage.getItem(sessionStorageKey(runId));
    } catch {
      return null;
    }
  },
  write(runId, sessionId) {
    if (typeof sessionStorage === "undefined") return;
    try {
      sessionStorage.setItem(sessionStorageKey(runId), sessionId);
    } catch {
      // ignore quota or privacy-mode failures; session will be recreated next time
    }
  },
  clear(runId) {
    if (typeof sessionStorage === "undefined") return;
    try {
      sessionStorage.removeItem(sessionStorageKey(runId));
    } catch {
      // best effort
    }
  },
};

export interface AskFabroAdapterOptions {
  /** Run ID this Ask Fabro session is scoped to. */
  runId: string;
  /** Catalog model id used when creating a fresh session. */
  defaultModel?: string | null;
  /** Override session persistence; defaults to `sessionStorage` keyed by run. */
  persistedSession?: PersistedSessionState;
  /** Override stream impl for tests. */
  streamSessionTurnImpl?: typeof streamSessionTurn;
  /** Override attach impl for tests. */
  attachSessionEventsImpl?: typeof attachSessionEvents;
  /** Override session API for tests. */
  createSession?: (
    runId: string,
    body: { title?: string; model?: string },
  ) => Promise<{ id: string }>;
}

/**
 * State accumulated as `run.session.*` events arrive during a single turn,
 * mapped to assistant-ui's `ThreadAssistantMessagePart[]` view model. The
 * assistant-ui runtime is given a snapshot after every event so users see
 * streaming text and tool-call cards in real time.
 */
interface TurnAccumulator {
  textParts: Array<{ text: string }>;
  /** Active text part index, if the last delta added/extended text. */
  activeTextIndex: number | null;
  parts: ThreadAssistantMessagePart[];
  /** Maps `tool_call_id` → index in `parts` for completing pairs. */
  toolCallIndex: Map<string, number>;
}

function emptyAccumulator(): TurnAccumulator {
  return {
    textParts: [],
    activeTextIndex: null,
    parts: [],
    toolCallIndex: new Map(),
  };
}

function snapshot(acc: TurnAccumulator): ChatModelRunResult {
  return { content: acc.parts.slice() };
}

/**
 * Apply a single `EventEnvelope` to the accumulator. Returns true if the
 * accumulator changed and a fresh `ChatModelRunResult` should be yielded.
 */
interface NestedRunEvent {
  event?: string;
  properties?: Record<string, unknown>;
}

export function applyTurnEvent(
  acc: TurnAccumulator,
  envelope: SessionStreamEvent,
): boolean {
  // The on-wire SSE envelope is `{ seq, event: { event: "...", properties } }`,
  // but the generated OpenAPI `EventEnvelope` type flattens the inner event
  // fields. Cast through `unknown` to read the nested runtime shape that the
  // server actually emits (matches `session-stream.test.ts`).
  const nested = (envelope as unknown as { event?: NestedRunEvent }).event ?? {};
  const eventName = nested.event ?? "";
  const props: Record<string, unknown> = nested.properties ?? {};

  if (eventName === "run.session.assistant_delta") {
    const delta = typeof props.delta === "string" ? props.delta : "";
    if (!delta) return false;
    if (acc.activeTextIndex == null) {
      const textPart = { text: delta };
      acc.textParts.push(textPart);
      acc.parts.push({ type: "text", text: delta });
      acc.activeTextIndex = acc.parts.length - 1;
    } else {
      const part = acc.parts[acc.activeTextIndex];
      if (part && part.type === "text") {
        const updated: ThreadAssistantMessagePart = {
          ...part,
          text: part.text + delta,
        };
        acc.parts[acc.activeTextIndex] = updated;
      }
    }
    return true;
  }

  if (eventName === "run.session.assistant_message") {
    // The full text was already streamed via deltas; the message event marks
    // the end of an assistant text segment. Reset the active-text pointer so
    // any following tool calls become separate parts, and any later text part
    // starts fresh (matches the durable transcript projection).
    if (acc.activeTextIndex != null) {
      acc.activeTextIndex = null;
      return true;
    }
    const text = typeof props.text === "string" ? props.text : "";
    if (text) {
      acc.parts.push({ type: "text", text });
      return true;
    }
    return false;
  }

  if (eventName === "run.session.tool_call.started") {
    const toolCallId = typeof props.tool_call_id === "string"
      ? props.tool_call_id
      : "";
    const toolName = typeof props.tool_name === "string" ? props.tool_name : "";
    if (!toolCallId || !toolName) return false;
    const argsValue = props.arguments;
    const args =
      argsValue && typeof argsValue === "object" ? (argsValue as object) : {};
    acc.parts.push({
      type: "tool-call",
      toolCallId,
      toolName,
      // Assistant-ui expects a JSON-shaped value here; the property's actual
      // shape is whatever the tool's argument schema produces.
      args: args as never,
      argsText: JSON.stringify(args),
    });
    acc.toolCallIndex.set(toolCallId, acc.parts.length - 1);
    acc.activeTextIndex = null;
    return true;
  }

  if (eventName === "run.session.tool_call.completed") {
    const toolCallId = typeof props.tool_call_id === "string"
      ? props.tool_call_id
      : "";
    if (!toolCallId) return false;
    const index = acc.toolCallIndex.get(toolCallId);
    if (index == null) return false;
    const part = acc.parts[index];
    if (!part || part.type !== "tool-call") return false;
    acc.parts[index] = { ...part, result: props.output };
    return true;
  }

  return false;
}

type CreateSession = NonNullable<AskFabroAdapterOptions["createSession"]>;

function defaultCreateSession(
  runId: string,
  body: { title?: string; model?: string },
): Promise<{ id: string }> {
  return sessionsApi
    .createRunSession(runId, body)
    .then((response) => ({ id: response.data.id }));
}

function lastUserText(
  messages: ReadonlyArray<{ role: string; content: ReadonlyArray<unknown> }>,
): string {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (!message || message.role !== "user") continue;
    const text = message.content
      .map((part) => {
        if (
          part &&
          typeof part === "object" &&
          (part as { type?: unknown }).type === "text" &&
          typeof (part as { text?: unknown }).text === "string"
        ) {
          return (part as { text: string }).text;
        }
        return "";
      })
      .filter(Boolean)
      .join("\n");
    if (text) return text;
  }
  return "";
}

/**
 * Build an assistant-ui `ChatModelAdapter` that talks to the Fabro Sessions
 * API. The adapter is parameterized by a `runId`; the session is created
 * lazily on the first turn (reusing a `sessionStorage`-cached id on reopen)
 * and turns are submitted via streamed SSE.
 */
export function createAskFabroAdapter(
  options: AskFabroAdapterOptions,
): ChatModelAdapter {
  const persisted = options.persistedSession ?? defaultPersistedSessionState;
  const streamImpl = options.streamSessionTurnImpl ?? streamSessionTurn;
  const createSession: CreateSession =
    options.createSession ?? defaultCreateSession;

  let sessionId: string | null = persisted.read(options.runId);

  async function ensureSession(): Promise<string> {
    if (sessionId) return sessionId;
    const body: { title?: string; model?: string } = { title: "Ask Fabro" };
    if (options.defaultModel) body.model = options.defaultModel;
    const created = await createSession(options.runId, body);
    sessionId = created.id;
    persisted.write(options.runId, sessionId);
    return sessionId;
  }

  return {
    async *run({ messages, abortSignal }) {
      const id = await ensureSession();
      const input = lastUserText(messages as never);

      const acc = emptyAccumulator();
      const queue: SessionStreamEvent[] = [];
      let resolveWaiter: (() => void) | null = null;
      let streamDone = false;
      let streamError: unknown = null;

      const streamPromise = streamImpl({
        sessionId: id,
        input,
        signal: abortSignal,
        onEvent: (event) => {
          queue.push(event);
          if (resolveWaiter) {
            const r = resolveWaiter;
            resolveWaiter = null;
            r();
          }
        },
      }).then(
        () => {
          streamDone = true;
          if (resolveWaiter) {
            const r = resolveWaiter;
            resolveWaiter = null;
            r();
          }
        },
        (err) => {
          streamError = err;
          streamDone = true;
          if (resolveWaiter) {
            const r = resolveWaiter;
            resolveWaiter = null;
            r();
          }
        },
      );

      while (true) {
        if (queue.length === 0) {
          if (streamDone) break;
          await new Promise<void>((resolve) => {
            resolveWaiter = resolve;
          });
          continue;
        }
        const event = queue.shift();
        if (!event) continue;
        if (applyTurnEvent(acc, event)) {
          yield snapshot(acc);
        }
      }

      await streamPromise;
      if (streamError) throw streamError;
      yield snapshot(acc);
    },
  };
}