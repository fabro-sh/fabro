import { afterEach, describe, expect, mock, test } from "bun:test";

import {
  ApiError,
  apiData,
  extractRequestId,
  fetchAllPages,
  generatedAxios,
  stageArtifactDownloadUrl,
} from "./api-client";

afterEach(() => {
  mock.restore();
  delete (globalThis as { window?: unknown }).window;
});

function axiosFailure({
  status,
  statusText,
  data,
  headers = {},
}: {
  status: number;
  statusText?: string;
  data?: unknown;
  headers?: Record<string, string>;
}) {
  return {
    isAxiosError: true,
    message: statusText ?? `HTTP ${status}`,
    response: {
      status,
      statusText: statusText ?? "",
      data,
      headers,
    },
  };
}

describe("generated Axios adapter", () => {
  test("uses same-origin requests with browser credentials", () => {
    expect(generatedAxios.defaults.baseURL).toBe("");
    expect(generatedAxios.defaults.withCredentials).toBe(true);
  });

  test("normalizes generated client failures into ApiError", async () => {
    const body = {
      errors: [{
        status: "500",
        title: "Internal",
        detail: "Database is unavailable.",
        request_id: "body-req",
      }],
    };

    try {
      await apiData(() =>
        Promise.reject(
          axiosFailure({
            status: 500,
            statusText: "Internal Server Error",
            data: body,
            headers: { "x-request-id": "header-req" },
          }),
        ),
      );
      throw new Error("expected apiData to reject");
    } catch (error) {
      expect(error).toBeInstanceOf(ApiError);
      expect(error).toMatchObject({
        status: 500,
        message: "Database is unavailable.",
        requestId: "header-req",
        body,
      });
    }
  });

  test("redirects normal authenticated 401 responses to login", async () => {
    const location = { href: "" };
    (globalThis as unknown as { window: { location: typeof location } }).window = {
      location,
    };

    await expect(() =>
      apiData(() => Promise.reject(axiosFailure({ status: 401 }))),
    ).toThrow(ApiError);

    expect(location.href).toBe("/login");
  });

  test("can suppress login redirects for login and install calls", async () => {
    const location = { href: "" };
    (globalThis as unknown as { window: { location: typeof location } }).window = {
      location,
    };

    await expect(() =>
      apiData(
        () => Promise.reject(axiosFailure({ status: 401 })),
        { redirectOnUnauthorized: false },
      ),
    ).toThrow(ApiError);

    expect(location.href).toBe("");
  });
});

describe("fetchAllPages", () => {
  test("preserves first-page extras and stops at the page cap", async () => {
    const warnMock = mock(() => {});
    const originalWarn = console.warn;
    console.warn = warnMock;
    let calls = 0;

    try {
      const result = await fetchAllPages<{ id: string }, { columns: { id: string; name: string }[] }>(
        "board runs",
        async () => {
          calls += 1;
          return {
            columns: [{ id: "running", name: "Running" }],
            data: [{ id: `run-${calls}` }],
            meta: { has_more: true },
          };
        },
      );

      expect(result.columns).toEqual([{ id: "running", name: "Running" }]);
      expect(result.data).toHaveLength(50);
      expect(result.meta.has_more).toBe(true);
      expect(warnMock).toHaveBeenCalledTimes(1);
    } finally {
      console.warn = originalWarn;
    }
  });
});

describe("stageArtifactDownloadUrl", () => {
  test("builds the escaped download href", () => {
    expect(
      stageArtifactDownloadUrl("run 1", "stage@1", "logs/output.txt", 2),
    ).toBe(
      "/api/v1/runs/run%201/stages/stage%401/artifacts/download?filename=logs%2Foutput.txt&retry=2",
    );
  });
});

describe("extractRequestId", () => {
  test("supports top-level, error-level, and detail-embedded request ids", () => {
    expect(extractRequestId({ request_id: "top" })).toBe("top");
    expect(extractRequestId({ errors: [{ request_id: "nested" }] })).toBe("nested");
    expect(extractRequestId({ errors: [{ detail: "Request ID: req-detail" }] })).toBe("req-detail");
  });
});
