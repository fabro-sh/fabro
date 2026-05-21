import type { EventEnvelope } from "@qltysh/fabro-api-client";

import { ApiError, extractRequestId } from "./api-client";

export type SessionStreamEvent = EventEnvelope;

type FetchLike = (
  input: string,
  init?: RequestInit,
) => Promise<Response>;

interface SessionStreamOptions {
  sessionId: string;
  signal?: AbortSignal;
  fetchImpl?: FetchLike;
  onEvent: (event: SessionStreamEvent) => void;
}

export interface StreamSessionTurnOptions extends SessionStreamOptions {
  input: string;
  turnId?: string;
}

export interface StreamSessionTurnResult {
  turnId: string | null;
}

export interface AttachSessionEventsOptions extends SessionStreamOptions {
  sinceSeq?: number;
}

export async function streamSessionTurn({
  sessionId,
  input,
  turnId,
  signal,
  fetchImpl = fetch,
  onEvent,
}: StreamSessionTurnOptions): Promise<StreamSessionTurnResult> {
  const body: { input: string; turn_id?: string } = { input };
  if (turnId) body.turn_id = turnId;

  const response = await fetchImpl(`/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`, {
    method: "POST",
    credentials: "same-origin",
    headers: {
      accept: "text/event-stream",
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
    signal,
  });
  await throwIfApiError(response);

  await readEventStream(response, onEvent);
  return { turnId: response.headers.get("x-fabro-turn-id") };
}

export async function attachSessionEvents({
  sessionId,
  sinceSeq,
  signal,
  fetchImpl = fetch,
  onEvent,
}: AttachSessionEventsOptions): Promise<void> {
  const params = new URLSearchParams();
  if (sinceSeq != null) params.set("since_seq", String(sinceSeq));
  const query = params.toString();
  const response = await fetchImpl(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/attach${query ? `?${query}` : ""}`,
    {
      method: "GET",
      credentials: "same-origin",
      headers: { accept: "text/event-stream" },
      signal,
    },
  );
  await throwIfApiError(response);

  await readEventStream(response, onEvent);
}

async function throwIfApiError(response: Response): Promise<void> {
  if (response.ok) return;

  const body = await readErrorBody(response);
  const requestId = response.headers.get("x-request-id") ?? extractRequestId(body);
  throw new ApiError({
    status: response.status,
    message: extractErrorDetail(body) ?? (response.statusText || `HTTP ${response.status}`),
    requestId,
    body,
  });
}

async function readErrorBody(response: Response): Promise<unknown> {
  const contentType = response.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) {
    return response.json().catch(() => null);
  }
  const text = await response.text().catch(() => "");
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

function extractErrorDetail(body: unknown): string | null {
  if (!body || typeof body !== "object") return null;
  const errors = (body as Record<string, unknown>).errors;
  if (!Array.isArray(errors) || errors.length === 0) return null;
  const first = errors[0];
  if (!first || typeof first !== "object") return null;
  const detail = (first as Record<string, unknown>).detail;
  return typeof detail === "string" && detail.length > 0 ? detail : null;
}

async function readEventStream(
  response: Response,
  onEvent: (event: SessionStreamEvent) => void,
): Promise<void> {
  if (!response.body) return;

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    buffer = drainSseBuffer(buffer, onEvent);
  }

  buffer += decoder.decode();
  drainSseBuffer(`${buffer}\n\n`, onEvent);
}

function drainSseBuffer(
  buffer: string,
  onEvent: (event: SessionStreamEvent) => void,
): string {
  let cursor = 0;
  while (true) {
    const next = buffer.indexOf("\n\n", cursor);
    if (next === -1) return buffer.slice(cursor);
    const frame = buffer.slice(cursor, next);
    cursor = next + 2;
    const data = frame
      .split(/\r?\n/)
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice("data:".length).trimStart())
      .join("\n");
    if (!data) continue;
    onEvent(JSON.parse(data) as SessionStreamEvent);
  }
}
