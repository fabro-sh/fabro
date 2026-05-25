import axios, {
  isAxiosError,
  type AxiosPromise,
  type AxiosResponse,
  type RawAxiosRequestConfig,
} from "axios";
import {
  AuthApi,
  Configuration,
  HumanInTheLoopApi,
  InsightsApi,
  InstallApi,
  ModelsApi,
  RunInternalsApi,
  RunInternalsApiAxiosParamCreator,
  RunOutputsApi,
  RunsApi,
  SecretsApi,
  SessionsApi,
  SettingsApi,
  SystemApi,
  WorkflowsApi,
} from "@qltysh/fabro-api-client";

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

interface ApiCallOptions {
  redirectOnUnauthorized?: boolean;
}

const PAGINATED_API_MAX_PAGES = 50;
const PAGINATED_API_MAX_ITEMS = 5000;

export const generatedAxios = axios.create({
  baseURL: "",
  withCredentials: true,
});

export const generatedApiConfiguration = new Configuration({
  basePath: "",
  baseOptions: {
    withCredentials: true,
  },
});

export const authApi = new AuthApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const humanInTheLoopApi = new HumanInTheLoopApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const insightsApi = new InsightsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const installApi = new InstallApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const modelsApi = new ModelsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const runInternalsApi = new RunInternalsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const runOutputsApi = new RunOutputsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const runsApi = new RunsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const secretsApi = new SecretsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const sessionsApi = new SessionsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const settingsApi = new SettingsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const systemApi = new SystemApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);
export const workflowsApi = new WorkflowsApi(
  generatedApiConfiguration,
  "",
  generatedAxios,
);

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

function requestIdFromHeaders(headers: unknown): string | null {
  return (
    headerValue(headers, "x-request-id")
    ?? headerValue(headers, "x-fabro-request-id")
    ?? headerValue(headers, "request-id")
  );
}

function headerValue(headers: unknown, name: string): string | null {
  if (!headers || typeof headers !== "object") return null;

  const getter = (headers as { get?: (key: string) => unknown }).get;
  if (typeof getter === "function") {
    const value = getter.call(headers, name);
    if (typeof value === "string") return value;
  }

  const wanted = name.toLowerCase();
  for (const [key, value] of Object.entries(headers as Record<string, unknown>)) {
    if (key.toLowerCase() !== wanted) continue;
    if (typeof value === "string") return value;
    if (Array.isArray(value) && typeof value[0] === "string") return value[0];
  }
  return null;
}

function apiErrorFromAxios(error: unknown): ApiError | null {
  if (!isAxiosError(error) || !error.response) return null;

  const { response } = error;
  const requestId = requestIdFromHeaders(response.headers) ?? extractRequestId(response.data);
  return new ApiError({
    status: response.status,
    message: extractErrorDetail(response.data) ?? (response.statusText || `HTTP ${response.status}`),
    requestId,
    body: response.data ?? null,
  });
}

export async function apiErrorFromFetchResponse(response: Response): Promise<ApiError | null> {
  if (response.ok) return null;

  const body = await readFetchErrorBody(response);
  const requestId = requestIdFromHeaders(response.headers) ?? extractRequestId(body);
  return new ApiError({
    status: response.status,
    message: extractErrorDetail(body) ?? (response.statusText || `HTTP ${response.status}`),
    requestId,
    body,
  });
}

async function readFetchErrorBody(response: Response): Promise<unknown> {
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

function redirectToLogin(error: ApiError, options: ApiCallOptions) {
  if (error.status !== 401 || options.redirectOnUnauthorized === false) return;
  if (typeof window !== "undefined") {
    window.location.href = "/login";
  }
}

export async function apiData<T>(
  call: () => AxiosPromise<T>,
  options: ApiCallOptions = {},
): Promise<T> {
  try {
    const response = await call();
    return response.data;
  } catch (error) {
    const apiError = apiErrorFromAxios(error);
    if (!apiError) throw error;
    redirectToLogin(apiError, options);
    throw apiError;
  }
}

export async function apiResponse<T>(
  call: () => AxiosPromise<T>,
  options: ApiCallOptions = {},
): Promise<AxiosResponse<T>> {
  try {
    return await call();
  } catch (error) {
    const apiError = apiErrorFromAxios(error);
    if (!apiError) throw error;
    redirectToLogin(apiError, options);
    throw apiError;
  }
}

export async function apiNullableData<T>(
  call: () => AxiosPromise<T>,
): Promise<T | null> {
  try {
    return await apiData(call);
  } catch (error) {
    if (error instanceof ApiError && isNotAvailable(error.status)) return null;
    throw error;
  }
}

export async function fetchAllPages<TItem, TExtra extends object = {}>(
  label: string,
  loadPage: (limit: number, offset: number) => Promise<PaginatedEnvelope<TItem> & TExtra>,
): Promise<PaginatedEnvelope<TItem> & TExtra> {
  const limit = 100;
  let offset = 0;
  const data: TItem[] = [];
  let extras: TExtra | null = null;
  let pagesLoaded = 0;

  while (true) {
    const page = await loadPage(limit, offset);
    if (extras == null) {
      const { data: _data, meta: _meta, ...rest } = page as PaginatedEnvelope<TItem> &
        Record<string, unknown>;
      extras = rest as TExtra;
    }

    pagesLoaded += 1;
    const pageData = page.data;
    const remainingItemBudget = PAGINATED_API_MAX_ITEMS - data.length;
    const pageItems = remainingItemBudget > 0 ? pageData.slice(0, remainingItemBudget) : [];
    data.push(...pageItems);

    if (!page.meta.has_more || pageData.length === 0) {
      return {
        ...(extras ?? ({} as TExtra)),
        data,
        meta: { has_more: false },
      };
    }

    if (
      pagesLoaded >= PAGINATED_API_MAX_PAGES
      || pageItems.length < pageData.length
      || data.length >= PAGINATED_API_MAX_ITEMS
    ) {
      console.warn(
        `Stopped paginated API fetch for ${label} after ${pagesLoaded} pages and ${data.length} items because the safety cap was reached.`,
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

export async function fetchAllStageEvents<TItem extends { seq: number }>(
  label: string,
  loadPage: (sinceSeq: number, limit: number) => Promise<PaginatedEnvelope<TItem>>,
): Promise<TItem[]> {
  const PAGE_LIMIT = 1000;
  const MAX_PAGES = 50;
  const data: TItem[] = [];
  let sinceSeq = 1;
  let pagesLoaded = 0;

  while (true) {
    const page = await loadPage(sinceSeq, PAGE_LIMIT);
    pagesLoaded += 1;

    if (page.data.length === 0) {
      if (page.meta.has_more) {
        console.warn(
          `Stage events fetch for ${label} returned an empty page with has_more=true; stopping at ${data.length} items to avoid spinning.`,
        );
      }
      return data;
    }

    data.push(...page.data);
    if (!page.meta.has_more) return data;

    if (pagesLoaded >= MAX_PAGES) {
      console.warn(
        `Stopped stage events fetch for ${label} after ${pagesLoaded} pages and ${data.length} items because the safety cap was reached.`,
      );
      return data;
    }

    const highestSeq = page.data.reduce((max, event) => Math.max(max, event.seq), sinceSeq - 1);
    if (highestSeq < sinceSeq) {
      console.warn(
        `Stage events fetch for ${label} returned a non-advancing page at since_seq=${sinceSeq}; stopping at ${data.length} items to avoid spinning.`,
      );
      return data;
    }
    sinceSeq = highestSeq + 1;
  }
}

export function requestSignalOptions(request?: Request): RawAxiosRequestConfig {
  return request?.signal ? { signal: request.signal } : {};
}

export async function stageArtifactDownloadUrl(
  id: string,
  stageId: string,
  filename: string,
  retry: number,
): Promise<string> {
  const requestArgs = await RunInternalsApiAxiosParamCreator(
    generatedApiConfiguration,
  ).getStageArtifact(id, stageId, filename, retry);
  return `${generatedApiConfiguration.basePath ?? ""}${requestArgs.url}`;
}
