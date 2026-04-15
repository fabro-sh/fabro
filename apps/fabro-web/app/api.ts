export interface ApiOptions {
  init?: RequestInit;
  request?: Request;
}

export async function apiFetch(path: string, options?: ApiOptions): Promise<Response> {
  const { init } = options ?? {};
  const response = await fetch(`/api/v1${path}`, {
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
  const response = await fetch("/api/v1/auth/config", { credentials: "include" });
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
  features: { session_sandboxes: boolean; retros: boolean };
}> {
  const response = await fetch("/api/v1/auth/me", { credentials: "include" });
  if (response.status === 401) {
    throw new Response(null, { status: 401, statusText: "Unauthorized" });
  }
  if (!response.ok) {
    throw new Response(null, { status: response.status, statusText: response.statusText });
  }
  return response.json();
}
