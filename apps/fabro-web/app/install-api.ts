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

import { ApiError, apiData, installApi } from "./lib/api-client";

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

export function readInstallApiError(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    const detail = installErrorDetail(error.body);
    if (detail) return detail;
    return `${fallback} (${error.status})`;
  }
  return error instanceof Error ? error.message : fallback;
}

function installErrorDetail(body: unknown): string | null {
  if (!body || typeof body !== "object") return null;
  const errors = (body as { errors?: unknown }).errors;
  if (!Array.isArray(errors)) return null;
  const first = errors[0];
  if (!first || typeof first !== "object") return null;
  const detail = (first as { detail?: unknown }).detail;
  return typeof detail === "string" && detail.trim() ? detail : null;
}

function installOptions(token: string) {
  return {
    headers: { Authorization: `Bearer ${token}` },
  };
}

async function installCall<T>(
  fallback: string,
  call: () => Promise<T>,
): Promise<T> {
  try {
    return await call();
  } catch (error) {
    throw new Error(readInstallApiError(error, fallback));
  }
}

export async function getInstallSession(token: string): Promise<InstallSessionResponse> {
  return installCall("install session request failed", () =>
    apiData(() => installApi.getInstallSession(installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function testInstallLlm(
  token: string,
  provider: InstallLlmProviderInput,
): Promise<void> {
  await installCall("install llm validation failed", () =>
    apiData(() => installApi.testInstallLlmCredentials(provider, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function putInstallLlm(
  token: string,
  providers: InstallLlmProviderInput[],
): Promise<void> {
  await installCall("install llm request failed", () =>
    apiData(() => installApi.putInstallLlm({ providers }, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function putInstallServer(token: string, canonicalUrl: string): Promise<void> {
  await installCall("install server request failed", () =>
    apiData(() => installApi.putInstallServer({ canonical_url: canonicalUrl }, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function testInstallObjectStore(
  token: string,
  input: InstallObjectStoreInput,
): Promise<void> {
  await installCall("install object store validation failed", () =>
    apiData(() => installApi.testInstallObjectStore(input, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function putInstallObjectStore(
  token: string,
  input: InstallObjectStoreInput,
): Promise<void> {
  await installCall("install object store request failed", () =>
    apiData(() => installApi.putInstallObjectStore(input, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function testInstallSandbox(
  token: string,
  input: InstallSandboxInput,
): Promise<void> {
  await installCall("install sandbox validation failed", () =>
    apiData(() => installApi.testInstallSandbox(input, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function putInstallSandbox(
  token: string,
  input: InstallSandboxInput,
): Promise<void> {
  await installCall("install sandbox request failed", () =>
    apiData(() => installApi.putInstallSandbox(input, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function testInstallGithubToken(
  token: string,
  githubToken: string,
): Promise<string> {
  const body = await installCall("install github token validation failed", () =>
    apiData(() => installApi.testInstallGithubToken({ token: githubToken }, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
  return body.username;
}

export async function putInstallGithubToken(
  token: string,
  githubToken: string,
  username: string,
): Promise<void> {
  await installCall("install github token request failed", () =>
    apiData(() => installApi.putInstallGithubToken(
      { token: githubToken, username },
      installOptions(token),
    ), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function createInstallGithubAppManifest(
  token: string,
  input: InstallGithubAppManifestInput,
): Promise<InstallGithubAppManifestResponse> {
  return installCall("install github app manifest request failed", () =>
    apiData(() => installApi.createInstallGithubAppManifest(input, installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}

export async function finishInstall(token: string): Promise<InstallFinishResponse> {
  return installCall("install finish request failed", () =>
    apiData(() => installApi.finishInstall(installOptions(token)), {
      redirectOnUnauthorized: false,
    }),
  );
}
