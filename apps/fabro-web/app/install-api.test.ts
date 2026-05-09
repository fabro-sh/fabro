import { afterEach, describe, expect, test } from "bun:test";
import type { AxiosAdapter, AxiosRequestConfig } from "axios";

import {
  putInstallObjectStore,
  putInstallSandbox,
  readInstallError,
  testInstallObjectStore,
  testInstallSandbox,
} from "./install-api";
import { generatedAxios } from "./lib/api-client";

type StubGeneratedResponse = {
  status: number;
  data?: unknown;
  statusText?: string;
};

const originalAdapter = generatedAxios.defaults.adapter;

afterEach(() => {
  generatedAxios.defaults.adapter = originalAdapter;
});

function stubGeneratedAxios(response: StubGeneratedResponse) {
  const calls: AxiosRequestConfig[] = [];
  generatedAxios.defaults.adapter = (async (config) => {
    calls.push(config);
    if (response.status >= 400) {
      throw {
        isAxiosError: true,
        message: response.statusText ?? `HTTP ${response.status}`,
        response: {
          status: response.status,
          statusText: response.statusText ?? "",
          data: response.data,
          headers: {},
        },
      };
    }
    return {
      data: response.data,
      status: response.status,
      statusText: response.statusText ?? "",
      headers: {},
      config,
    };
  }) as AxiosAdapter;
  return calls;
}

function headerValue(config: AxiosRequestConfig, name: string): string | undefined {
  const headers = config.headers as { get?: (key: string) => unknown } | undefined;
  const value = headers?.get?.(name);
  return typeof value === "string" ? value : undefined;
}

describe("readInstallError", () => {
  test("prefers the structured install error payload", async () => {
    const response = new Response(
      JSON.stringify({
        errors: [{ status: "422", title: "Unprocessable Entity", detail: "invalid token" }],
      }),
      {
        status: 422,
        headers: { "Content-Type": "application/json" },
      },
    );

    await expect(
      readInstallError(response, "install request failed"),
    ).resolves.toBe("invalid token");
  });

  test("falls back to the provided message when the body is not structured JSON", async () => {
    const response = new Response("boom", {
      status: 500,
      headers: { "Content-Type": "text/plain" },
    });

    await expect(
      readInstallError(response, "install request failed"),
    ).resolves.toBe("install request failed (500)");
  });
});

describe("install object-store requests", () => {
  test("testInstallObjectStore posts the install payload to the validation endpoint", async () => {
    const calls = stubGeneratedAxios({ status: 200, data: { ok: true } });

    await testInstallObjectStore("test-install-token", {
      provider: "s3",
      bucket: "fabro-data",
      region: "us-east-1",
      credential_mode: "runtime",
    });

    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("/install/object-store/test");
    expect(calls[0]!.method).toBe("post");
    expect(headerValue(calls[0]!, "Authorization")).toBe("Bearer test-install-token");
    expect(headerValue(calls[0]!, "Content-Type")).toContain("application/json");
    expect(calls[0]!.data).toBe(
      JSON.stringify({
        provider: "s3",
        bucket: "fabro-data",
        region: "us-east-1",
        credential_mode: "runtime",
      }),
    );
  });

  test("putInstallObjectStore surfaces structured API errors", async () => {
    stubGeneratedAxios({
      status: 422,
      statusText: "Unprocessable Entity",
      data: {
        errors: [
          {
            status: "422",
            title: "Unprocessable Entity",
            detail: "Bucket is required.",
          },
        ],
      },
    });

    await expect(
      putInstallObjectStore("test-install-token", { provider: "s3" }),
    ).rejects.toThrow("Bucket is required.");
  });
});

describe("install sandbox requests", () => {
  test("testInstallSandbox posts the install payload to the validation endpoint", async () => {
    const calls = stubGeneratedAxios({ status: 200, data: { ok: true } });

    await testInstallSandbox("test-install-token", {
      provider: "daytona",
      api_key: "dtn_test",
    });

    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("/install/sandbox/test");
    expect(calls[0]!.method).toBe("post");
    expect(calls[0]!.data).toBe(
      JSON.stringify({ provider: "daytona", api_key: "dtn_test" }),
    );
  });

  test("putInstallSandbox surfaces structured API errors", async () => {
    stubGeneratedAxios({
      status: 422,
      statusText: "Unprocessable Entity",
      data: {
        errors: [
          {
            status: "422",
            title: "Unprocessable Entity",
            detail: "api_key is required for daytona",
          },
        ],
      },
    });

    await expect(
      putInstallSandbox("test-install-token", { provider: "daytona" }),
    ).rejects.toThrow("api_key is required for daytona");
  });
});
