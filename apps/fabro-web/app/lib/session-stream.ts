import {
  SessionsApiAxiosParamCreator,
  type EventEnvelope,
  type SubmitTurnRequest,
} from "@qltysh/fabro-api-client";

import {
  apiErrorFromFetchResponse,
  generatedApiConfiguration,
} from "./api-client";

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
  const body: SubmitTurnRequest = { input };
  if (turnId) body.turn_id = turnId;

  const request = await SessionsApiAxiosParamCreator(
    generatedApiConfiguration,
  ).submitSessionTurn(sessionId, body, { signal });
  const response = await fetchImpl(request.url, fetchInitFromAxiosRequest(request.options));
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
  const request = await SessionsApiAxiosParamCreator(
    generatedApiConfiguration,
  ).attachSessionEvents(sessionId, sinceSeq, { signal });
  const response = await fetchImpl(request.url, fetchInitFromAxiosRequest(request.options));
  await throwIfApiError(response);

  await readEventStream(response, onEvent);
}

async function throwIfApiError(response: Response): Promise<void> {
  const error = await apiErrorFromFetchResponse(response);
  if (error) throw error;
}

function fetchInitFromAxiosRequest(options: {
  method?: string;
  headers?: unknown;
  data?: unknown;
  signal?: unknown;
}): RequestInit {
  const init: RequestInit = {
    method: options.method,
    credentials: "same-origin",
    headers: options.headers as HeadersInit,
    signal: options.signal as AbortSignal | undefined,
  };
  if (options.data !== undefined) {
    init.body = typeof options.data === "string"
      ? options.data
      : JSON.stringify(options.data);
  }
  return init;
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
    // react-doctor-disable-next-line react-doctor/async-await-in-loop -- Streaming readers must consume chunks sequentially to preserve SSE order and decoder state.
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
    const match = /\r?\n\r?\n/g.exec(buffer.slice(cursor));
    if (!match) return buffer.slice(cursor);
    const next = cursor + match.index;
    const frame = buffer.slice(cursor, next);
    cursor = next + match[0].length;
    const data = frame
      .split(/\r?\n/)
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice("data:".length).trimStart())
      .join("\n");
    if (!data) continue;
    onEvent(JSON.parse(data) as SessionStreamEvent);
  }
}
