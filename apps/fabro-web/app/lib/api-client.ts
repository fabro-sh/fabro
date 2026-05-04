export interface ApiOptions {
  init?: RequestInit;
  request?: Request;
}

export interface PaginatedEnvelope<T> {
  data: T[];
  meta: { has_more: boolean };
}

export class ApiError extends Error {
  readonly status: number;
  readonly requestId: string | null;
  readonly body: unknown;

  constructor({
    status,
    message,
    requestId,
    body,
  }: {
    status: number;
    message: string;
    requestId: string | null;
    body: unknown;
  }) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.requestId = requestId;
    this.body = body;
  }
}

const API_PREFIX = "/api/v1";
const PAGINATED_API_MAX_PAGES = 50;
const PAGINATED_API_MAX_ITEMS = 5000;

export function apiPath(path: string): string {
  return path.startsWith(API_PREFIX) ? path : `${API_PREFIX}${path}`;
}

export function isNotAvailable(status: number): boolean {
  return status === 404 || status === 501;
}

export function extractRequestId(body: unknown): string | null {
  if (!body || typeof body !== "object") return null;
  const record = body as Record<string, unknown>;
  if (typeof record.request_id === "string") return record.request_id;
  if (typeof record.requestId === "string") return record.requestId;

  const errors = record.errors;
  if (!Array.isArray(errors) || errors.length === 0) return null;

  const first = errors[0];
  if (!first || typeof first !== "object") return null;
  const error = first as Record<string, unknown>;
  if (typeof error.request_id === "string") return error.request_id;
  if (typeof error.requestId === "string") return error.requestId;
  if (typeof error.detail === "string") {
    const match = error.detail.match(/request[_ ]id[=:]?\s*([a-zA-Z0-9-_]+)/i);
    if (match) return match[1];
  }
  return null;
}

function requestIdFromHeaders(headers: Headers): string | null {
  return (
    headers.get("x-request-id") ??
    headers.get("x-fabro-request-id") ??
    headers.get("request-id")
  );
}

async function parseResponseBody(response: Response): Promise<unknown> {
  const contentType = response.headers.get("content-type") ?? "";
  const text = await response.text().catch(() => "");
  if (!text) return null;
  if (contentType.includes("json")) {
    try {
      return JSON.parse(text);
    } catch {
      return text;
    }
  }
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

async function apiErrorFromResponse(response: Response): Promise<ApiError> {
  const body = await parseResponseBody(response);
  const requestId = requestIdFromHeaders(response.headers) ?? extractRequestId(body);
  return new ApiError({
    status: response.status,
    message: response.statusText || `HTTP ${response.status}`,
    requestId,
    body,
  });
}

export async function apiRequest(path: string, options?: ApiOptions): Promise<Response> {
  const { init, request } = options ?? {};
  const response = await fetch(apiPath(path), {
    ...init,
    credentials: "include",
    headers: init?.headers,
    ...(request?.signal ? { signal: request.signal } : {}),
  });

  if (response.status === 401) {
    if (typeof window !== "undefined") {
      window.location.href = "/login";
    }
    throw await apiErrorFromResponse(response);
  }

  return response;
}

export async function apiFetcher<T>(key: string): Promise<T> {
  const response = await apiRequest(key);
  if (!response.ok) {
    throw await apiErrorFromResponse(response);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

export async function apiTextFetcher(key: string): Promise<string> {
  const response = await apiRequest(key);
  if (!response.ok) {
    throw await apiErrorFromResponse(response);
  }
  return response.text();
}

export async function apiNullableFetcher<T>(key: string): Promise<T | null> {
  const response = await apiRequest(key);
  if (isNotAvailable(response.status)) return null;
  if (!response.ok) {
    throw await apiErrorFromResponse(response);
  }
  return response.json() as Promise<T>;
}

export async function apiNullableTextFetcher(key: string): Promise<string | null> {
  const response = await apiRequest(key);
  if (isNotAvailable(response.status)) return null;
  if (!response.ok) {
    throw await apiErrorFromResponse(response);
  }
  return response.text();
}

function paginatedApiPath(key: string, limit: number, offset: number): string {
  const url = new URL(apiPath(key), "http://fabro.local");
  url.searchParams.set("page[limit]", String(limit));
  url.searchParams.set("page[offset]", String(offset));
  return `${url.pathname}${url.search}`;
}

export async function apiPaginatedFetcher<TItem, TExtra extends object = {}>(
  key: string,
): Promise<PaginatedEnvelope<TItem> & TExtra> {
  const limit = 100;
  let offset = 0;
  const data: TItem[] = [];
  let extras: TExtra | null = null;
  let pagesLoaded = 0;

  while (true) {
    const response = await apiRequest(paginatedApiPath(key, limit, offset));
    if (!response.ok) {
      throw await apiErrorFromResponse(response);
    }

    const page = (await response.json()) as PaginatedEnvelope<TItem> & TExtra;
    if (extras == null) {
      const { data: _data, meta: _meta, ...rest } = page as PaginatedEnvelope<TItem> &
        Record<string, unknown>;
      extras = rest as TExtra;
    }

    pagesLoaded += 1;
    const remainingItemBudget = PAGINATED_API_MAX_ITEMS - data.length;
    const pageItems = remainingItemBudget > 0 ? page.data.slice(0, remainingItemBudget) : [];
    data.push(...pageItems);

    if (!page.meta.has_more || page.data.length === 0) {
      return {
        ...(extras ?? ({} as TExtra)),
        data,
        meta: { has_more: false },
      };
    }

    if (
      pagesLoaded >= PAGINATED_API_MAX_PAGES ||
      pageItems.length < page.data.length ||
      data.length >= PAGINATED_API_MAX_ITEMS
    ) {
      console.warn(
        `Stopped paginated API fetch for ${key} after ${pagesLoaded} pages and ${data.length} items because the safety cap was reached.`,
      );
      return {
        ...(extras ?? ({} as TExtra)),
        data,
        meta: { has_more: true },
      };
    }

    offset += page.data.length;
  }
}

function stageEventsPagePath(key: string, sinceSeq: number, limit: number): string {
  return `${apiPath(key)}?since_seq=${sinceSeq}&limit=${limit}`;
}

/**
 * Cursor-paginated fetcher for `/runs/{id}/stages/{stageId}/events`.
 *
 * Loops from `since_seq=1` with a 1000-event page size, advancing the cursor
 * to `highestSeq + 1` until the server reports `meta.has_more === false`.
 * The empty-page guard mirrors `apiPaginatedFetcher`: if the server claims
 * `has_more` but returns no rows we exit and `console.warn` to surface the
 * server invariant violation without spinning the UI.
 */
export async function fetchAllStageEvents<TItem>(key: string): Promise<TItem[]> {
  const PAGE_LIMIT = 1000;
  const MAX_PAGES = 50;
  const data: TItem[] = [];
  let sinceSeq = 1;
  let pagesLoaded = 0;

  while (true) {
    const response = await apiRequest(stageEventsPagePath(key, sinceSeq, PAGE_LIMIT));
    if (!response.ok) {
      throw await apiErrorFromResponse(response);
    }
    const page = (await response.json()) as PaginatedEnvelope<TItem & { seq: number }>;
    pagesLoaded += 1;

    if (page.data.length === 0) {
      if (page.meta.has_more) {
        console.warn(
          `Stage events fetch for ${key} returned an empty page with has_more=true; stopping at ${data.length} items to avoid spinning.`,
        );
      }
      return data;
    }

    data.push(...page.data);
    if (!page.meta.has_more) return data;

    if (pagesLoaded >= MAX_PAGES) {
      console.warn(
        `Stopped stage events fetch for ${key} after ${pagesLoaded} pages and ${data.length} items because the safety cap was reached.`,
      );
      return data;
    }

    const lastSeq = page.data[page.data.length - 1].seq;
    sinceSeq = lastSeq + 1;
  }
}

export async function apiJsonMutation<TResponse, TArg = unknown>(
  key: string,
  { arg }: { arg: TArg },
): Promise<TResponse> {
  const response = await apiRequest(key, {
    init: {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: arg === undefined ? undefined : JSON.stringify(arg),
    },
  });
  if (!response.ok) {
    throw await apiErrorFromResponse(response);
  }
  if (response.status === 204) return undefined as TResponse;
  return response.json() as Promise<TResponse>;
}