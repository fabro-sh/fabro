import type {
  InstallFinishResponse,
  InstallGithubAppManifestInput,
  InstallGithubAppManifestResponse,
  InstallGithubAppOwner,
  InstallLlmProviderInput,
  InstallObjectStoreInput,
  InstallObjectStoreSummary,
  InstallSandboxInput,
  InstallSandboxSummary,
  InstallSessionResponse,
} from "@qltysh/fabro-api-client";

export type {
  InstallFinishResponse,
  InstallGithubAppManifestInput,
  InstallGithubAppManifestResponse,
  InstallGithubAppOwner,
  InstallLlmProviderInput,
  InstallObjectStoreInput,
  InstallObjectStoreSummary,
  InstallSandboxInput,
  InstallSandboxSummary,
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

type InstallRequestOptions = {
  path:          string;
  method:        "GET" | "POST" | "PUT";
  body?:         unknown;
  errorFallback: string;
};

async function installRequest(
  token: string,
  opts: InstallRequestOptions,
): Promise<Response> {
  const headers: Record<string, string> = { Authorization: `Bearer ${token}` };
  if (opts.body !== undefined) {
    headers["Content-Type"] = "application/json";
  }
  const response = await fetch(opts.path, {
    method: opts.method,
    headers,
    body:   opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
  });
  if (!response.ok) {
    throw new Error(await readInstallError(response, opts.errorFallback));
  }
  return response;
}

async function installJsonRequest<T>(
  token: string,
  opts: InstallRequestOptions,
): Promise<T> {
  const response = await installRequest(token, opts);
  return response.json() as Promise<T>;
}

export async function getInstallSession(token: string): Promise<InstallSessionResponse> {
  return installJsonRequest<InstallSessionResponse>(token, {
    path:          "/install/session",
    method:        "GET",
    errorFallback: "install session request failed",
  });
}

export async function testInstallLlm(
  token: string,
  provider: InstallLlmProviderInput,
): Promise<void> {
  await installRequest(token, {
    path:          "/install/llm/test",
    method:        "POST",
    body:          provider,
    errorFallback: "install llm validation failed",
  });
}

export async function putInstallLlm(
  token: string,
  providers: InstallLlmProviderInput[],
): Promise<void> {
  await installRequest(token, {
    path:          "/install/llm",
    method:        "PUT",
    body:          { providers },
    errorFallback: "install llm request failed",
  });
}

export async function putInstallServer(token: string, canonicalUrl: string): Promise<void> {
  await installRequest(token, {
    path:          "/install/server",
    method:        "PUT",
    body:          { canonical_url: canonicalUrl },
    errorFallback: "install server request failed",
  });
}

export async function testInstallObjectStore(
  token: string,
  input: InstallObjectStoreInput,
): Promise<void> {
  await installRequest(token, {
    path:          "/install/object-store/test",
    method:        "POST",
    body:          input,
    errorFallback: "install object store validation failed",
  });
}

export async function putInstallObjectStore(
  token: string,
  input: InstallObjectStoreInput,
): Promise<void> {
  await installRequest(token, {
    path:          "/install/object-store",
    method:        "PUT",
    body:          input,
    errorFallback: "install object store request failed",
  });
}

export async function testInstallSandbox(
  token: string,
  input: InstallSandboxInput,
): Promise<void> {
  await installRequest(token, {
    path:          "/install/sandbox/test",
    method:        "POST",
    body:          input,
    errorFallback: "install sandbox validation failed",
  });
}

export async function putInstallSandbox(
  token: string,
  input: InstallSandboxInput,
): Promise<void> {
  await installRequest(token, {
    path:          "/install/sandbox",
    method:        "PUT",
    body:          input,
    errorFallback: "install sandbox request failed",
  });
}

export async function testInstallGithubToken(
  token: string,
  githubToken: string,
): Promise<string> {
  const body = await installJsonRequest<{ username: string }>(token, {
    path:          "/install/github/token/test",
    method:        "POST",
    body:          { token: githubToken },
    errorFallback: "install github token validation failed",
  });
  return body.username;
}

export async function putInstallGithubToken(
  token: string,
  githubToken: string,
  username: string,
): Promise<void> {
  await installRequest(token, {
    path:          "/install/github/token",
    method:        "PUT",
    body:          { token: githubToken, username },
    errorFallback: "install github token request failed",
  });
}

export async function createInstallGithubAppManifest(
  token: string,
  input: InstallGithubAppManifestInput,
): Promise<InstallGithubAppManifestResponse> {
  return installJsonRequest<InstallGithubAppManifestResponse>(token, {
    path:          "/install/github/app/manifest",
    method:        "POST",
    body:          input,
    errorFallback: "install github app manifest request failed",
  });
}

export async function finishInstall(token: string): Promise<InstallFinishResponse> {
  return installJsonRequest<InstallFinishResponse>(token, {
    path:          "/install/finish",
    method:        "POST",
    errorFallback: "install finish request failed",
  });
}
