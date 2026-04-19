export interface ApiOptions {
  init?: RequestInit;
  request?: Request;
}

export interface PaginatedEnvelope<T> {
  data: T[];
  meta: { has_more: boolean };
}

const PAGINATED_API_MAX_PAGES = 50;
const PAGINATED_API_MAX_ITEMS = 5000;

function buildApiPath(path: string): string {
  return `/api/v1${path}`;
}

function buildPaginatedApiPath(path: string, limit: number, offset: number): string {
  const url = new URL(buildApiPath(path), "http://fabro.local");
  url.searchParams.set("page[limit]", String(limit));
  url.searchParams.set("page[offset]", String(offset));
  return `${url.pathname}${url.search}`;
}

export async function apiFetch(path: string, options?: ApiOptions): Promise<Response> {
  const { init } = options ?? {};
  const response = await fetch(buildApiPath(path), {
    ...init,
    credentials: "include",
    headers: init?.headers,
  });

  if (response.status === 401) {
    window.location.href = "/login";
    throw new Error("Unauthorized");
  }

  return response;
}

export async function apiJson<T>(path: string, options?: ApiOptions): Promise<T> {
  const response = await apiFetch(path, options);
  if (!response.ok) {
    throw new Response(null, { status: response.status, statusText: response.statusText });
  }
  return response.json() as Promise<T>;
}

export async function apiPaginatedJson<TItem, TExtra extends object = {}>(
  path: string,
  options?: ApiOptions,
): Promise<PaginatedEnvelope<TItem> & TExtra> {
  const limit = 100;
  let offset = 0;
  const data: TItem[] = [];
  let extras: TExtra | null = null;
  let pagesLoaded = 0;

  while (true) {
    const response = await fetch(buildPaginatedApiPath(path, limit, offset), {
      ...options?.init,
      credentials: "include",
      headers: options?.init?.headers,
    });

    if (response.status === 401) {
      window.location.href = "/login";
      throw new Error("Unauthorized");
    }
    if (!response.ok) {
      throw new Response(null, { status: response.status, statusText: response.statusText });
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
        `Stopped paginated API fetch for ${path} after ${pagesLoaded} pages and ${data.length} items because the safety cap was reached.`,
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

export function isNotAvailable(status: number): boolean {
  return status === 404 || status === 501;
}

export async function apiJsonOrNull<T>(
  path: string,
  options?: ApiOptions,
): Promise<T | null> {
  const response = await apiFetch(path, options);
  if (isNotAvailable(response.status)) {
    return null;
  }
  if (!response.ok) {
    throw new Response(null, {
      status: response.status,
      statusText: response.statusText,
    });
  }
  return response.json() as Promise<T>;
}

export async function getAuthConfig(): Promise<{ methods: string[] }> {
  const response = await fetch(buildApiPath("/auth/config"), { credentials: "include" });
  if (!response.ok) {
    throw new Response(null, { status: response.status, statusText: response.statusText });
  }
  return response.json();
}

export async function loginDevToken(token: string): Promise<{ ok: boolean }> {
  const response = await fetch("/auth/login/dev-token", {
    method: "POST",
    credentials: "include",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ token }),
  });
  if (!response.ok) {
    throw new Response(null, { status: response.status, statusText: response.statusText });
  }
  return response.json();
}

export async function getAuthMe(): Promise<{
  user: {
    login: string;
    name: string;
    email: string;
    avatarUrl: string;
    userUrl: string;
  };
  provider: string;
  demoMode: boolean;
}> {
  const response = await fetch(buildApiPath("/auth/me"), { credentials: "include" });
  if (response.status === 401) {
    throw new Response(null, { status: 401, statusText: "Unauthorized" });
  }
  if (!response.ok) {
    throw new Response(null, { status: response.status, statusText: response.statusText });
  }
  return response.json();
}

export async function getSystemInfo(): Promise<{
  features: { session_sandboxes: boolean; retros: boolean };
}> {
  return apiJson("/system/info");
}
