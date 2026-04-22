import { describe, expect, test } from "bun:test";

import {
  putInstallObjectStore,
  readInstallError,
  testInstallObjectStore,
} from "./install-api";

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
    const calls: Array<{ input: RequestInfo | URL; init?: RequestInit }> = [];
    globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ input, init });
      return Promise.resolve(new Response(JSON.stringify({ ok: true }), { status: 200 }));
    }) as typeof fetch;

    await testInstallObjectStore("test-install-token", {
      provider: "s3",
      bucket: "fabro-data",
      region: "us-east-1",
      credential_mode: "runtime",
    });

    expect(calls).toHaveLength(1);
    expect(String(calls[0]!.input)).toBe("/install/object-store/test");
    expect(calls[0]!.init?.method).toBe("POST");
    expect(calls[0]!.init?.headers).toEqual({
      Authorization: "Bearer test-install-token",
      "Content-Type": "application/json",
    });
    expect(calls[0]!.init?.body).toBe(
      JSON.stringify({
        provider: "s3",
        bucket: "fabro-data",
        region: "us-east-1",
        credential_mode: "runtime",
      }),
    );
  });

  test("putInstallObjectStore surfaces structured API errors", async () => {
    globalThis.fetch = (() =>
      Promise.resolve(
        new Response(
          JSON.stringify({
            errors: [
              {
                status: "422",
                title: "Unprocessable Entity",
                detail: "Bucket is required.",
              },
            ],
          }),
          {
            status: 422,
            headers: { "Content-Type": "application/json" },
          },
        ),
      )) as typeof fetch;

    await expect(
      putInstallObjectStore("test-install-token", { provider: "s3" }),
    ).rejects.toThrow("Bucket is required.");
  });
});
