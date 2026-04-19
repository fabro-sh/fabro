import type {
  InstallFinishResponse,
  InstallGithubAppManifestInput,
  InstallGithubAppManifestResponse,
  InstallGithubAppOwner,
  InstallLlmProviderInput,
  InstallSessionResponse,
} from "@qltysh/fabro-api-client";

export type {
  InstallFinishResponse,
  InstallGithubAppManifestInput,
  InstallGithubAppManifestResponse,
  InstallGithubAppOwner,
  InstallLlmProviderInput,
  InstallSessionResponse,
};

const INSTALL_TOKEN_KEY = "fabro-install-token";

export function readStoredInstallToken(): string | null {
  try {
    return window.sessionStorage.getItem(INSTALL_TOKEN_KEY);
  } catch {
    return null;
  }
}

export function persistInstallToken(token: string | null): void {
  try {
    if (token) {
      window.sessionStorage.setItem(INSTALL_TOKEN_KEY, token);
    } else {
      window.sessionStorage.removeItem(INSTALL_TOKEN_KEY);
    }
  } catch {
    // best-effort only
  }
}

async function installFetch(path: string, token: string, init?: RequestInit): Promise<Response> {
  return fetch(path, {
    ...init,
    headers: {
      ...(init?.headers ?? {}),
      Authorization: `Bearer ${token}`,
    },
  });
}

export async function readInstallError(
  response: Response,
  fallback: string,
): Promise<string> {
  try {
    const body = (await response.clone().json()) as {
      errors?: Array<{ detail?: string }>;
    };
    const detail = body.errors?.[0]?.detail;
    if (detail) return detail;
  } catch {
    // fall through to the default message
  }
  return `${fallback} (${response.status})`;
}

export function buildInstallGithubAppOwner(
  ownerKind: "personal" | "org",
  organizationSlug: string,
): InstallGithubAppOwner {
  return ownerKind === "org"
    ? { kind: "org", slug: organizationSlug.trim() }
    : { kind: "personal" };
}

export async function getInstallSession(token: string): Promise<InstallSessionResponse> {
  const response = await installFetch("/install/session", token);
  if (!response.ok) {
    throw new Error(await readInstallError(response, "install session request failed"));
  }
  return response.json() as Promise<InstallSessionResponse>;
}

export async function testInstallLlm(
  token: string,
  provider: InstallLlmProviderInput,
): Promise<void> {
  const response = await installFetch("/install/llm/test", token, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(provider),
  });
  if (!response.ok) {
    throw new Error(await readInstallError(response, "install llm validation failed"));
  }
}

export async function putInstallLlm(
  token: string,
  providers: InstallLlmProviderInput[],
): Promise<void> {
  const response = await installFetch("/install/llm", token, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ providers }),
  });
  if (!response.ok) {
    throw new Error(await readInstallError(response, "install llm request failed"));
  }
}

export async function putInstallServer(token: string, canonicalUrl: string): Promise<void> {
  const response = await installFetch("/install/server", token, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ canonical_url: canonicalUrl }),
  });
  if (!response.ok) {
    throw new Error(await readInstallError(response, "install server request failed"));
  }
}

export async function testInstallGithubToken(
  token: string,
  githubToken: string,
): Promise<string> {
  const response = await installFetch("/install/github/token/test", token, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ token: githubToken }),
  });
  if (!response.ok) {
    throw new Error(
      await readInstallError(response, "install github token validation failed"),
    );
  }
  const body = (await response.json()) as { username: string };
  return body.username;
}

export async function putInstallGithubToken(
  token: string,
  githubToken: string,
  username: string,
): Promise<void> {
  const response = await installFetch("/install/github/token", token, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ token: githubToken, username }),
  });
  if (!response.ok) {
    throw new Error(await readInstallError(response, "install github token request failed"));
  }
}

export async function createInstallGithubAppManifest(
  token: string,
  input: InstallGithubAppManifestInput,
): Promise<InstallGithubAppManifestResponse> {
  const response = await installFetch("/install/github/app/manifest", token, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  if (!response.ok) {
    throw new Error(
      await readInstallError(response, "install github app manifest request failed"),
    );
  }
  return response.json() as Promise<InstallGithubAppManifestResponse>;
}

export async function finishInstall(token: string): Promise<InstallFinishResponse> {
  const response = await installFetch("/install/finish", token, {
    method: "POST",
  });
  if (!response.ok) {
    throw new Error(await readInstallError(response, "install finish request failed"));
  }
  return response.json() as Promise<InstallFinishResponse>;
}
