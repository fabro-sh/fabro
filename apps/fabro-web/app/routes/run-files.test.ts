import { afterEach, describe, expect, test } from "bun:test";

import {
  deepLinkToastMessage,
  emptyTransitionToastMessage,
  extractRequestId,
  loader,
} from "./run-files";

type StubResponseInit = {
  status:  number;
  body?:   string;
  headers?: Record<string, string>;
};

function buildRunFilesPayload({
  files = [],
  degraded = false,
  patch = null,
}: {
  files?: string[];
  degraded?: boolean;
  patch?: string | null;
}) {
  return {
    data: files.map((name) => ({
      change_kind: "modified",
      old_file: { name },
      new_file: { name },
    })),
    meta: {
      degraded,
      patch,
      total_changed: files.length,
      truncated: false,
    },
  } as any;
}

function stubFetchOnce(init: StubResponseInit) {
  const original = globalThis.fetch;
  globalThis.fetch = (() => {
    const response = new Response(init.body ?? "", {
      status:  init.status,
      headers: init.headers,
      // `Response` constructor derives statusText from status for known
      // codes; providing it explicitly keeps tests deterministic across
      // engines that disagree on the default message.
      statusText: init.status === 500 ? "Internal Server Error" : "",
    });
    return Promise.resolve(response);
  }) as typeof fetch;
  return () => {
    globalThis.fetch = original;
  };
}

describe("extractRequestId", () => {
  test("reads `request_id` from the top level of the error body", () => {
    expect(extractRequestId({ request_id: "abc-123" })).toBe("abc-123");
  });

  test("reads `request_id` from errors[0] under the uniform envelope", () => {
    expect(
      extractRequestId({
        errors: [
          { status: "500", title: "Internal", request_id: "evt_42" },
        ],
      }),
    ).toBe("evt_42");
  });

  test("parses `Request ID: xyz` out of errors[0].detail", () => {
    expect(
      extractRequestId({
        errors: [
          {
            status: "500",
            title:  "Internal Server Error",
            detail: "Run files failed. Request ID: req_999 on shard 2.",
          },
        ],
      }),
    ).toBe("req_999");
  });

  test("returns null for bodies without any request_id", () => {
    expect(extractRequestId(null)).toBe(null);
    expect(extractRequestId(undefined)).toBe(null);
    expect(extractRequestId("not an object")).toBe(null);
    expect(extractRequestId({ errors: [] })).toBe(null);
    expect(extractRequestId({ errors: [{ detail: "no id in here" }] })).toBe(
      null,
    );
  });

  test("handles request_id values with hyphens and underscores", () => {
    expect(
      extractRequestId({
        errors: [
          { detail: "Something failed. request_id: RX-1A_2B-3C4D" },
        ],
      }),
    ).toBe("RX-1A_2B-3C4D");
  });
});

describe("emptyTransitionToastMessage", () => {
  test("returns the no-changes toast when a populated diff becomes empty", () => {
    expect(emptyTransitionToastMessage(3, 0)).toBe("No changes in this run.");
  });

  test("returns null when the diff was already empty", () => {
    expect(emptyTransitionToastMessage(0, 0)).toBeNull();
    expect(emptyTransitionToastMessage(null, 0)).toBeNull();
    expect(emptyTransitionToastMessage(2, 1)).toBeNull();
  });
});

describe("deepLinkToastMessage", () => {
  test("returns the patch-only message when file navigation is unavailable", () => {
    expect(
      deepLinkToastMessage(
        "src/app.tsx",
        buildRunFilesPayload({
          degraded: true,
          patch: "@@ -1 +1 @@",
        }),
      ),
    ).toBe("File-level navigation isn't available in the patch-only view.");
  });

  test("returns the missing-file message when the requested file is absent", () => {
    expect(
      deepLinkToastMessage(
        "src/missing.ts",
        buildRunFilesPayload({ files: ["src/present.ts"] }),
      ),
    ).toBe("File src/missing.ts is not in this run.");
  });

  test("returns null when the deep-linked file exists", () => {
    expect(
      deepLinkToastMessage(
        "src/present.ts",
        buildRunFilesPayload({ files: ["src/present.ts"] }),
      ),
    ).toBeNull();
  });
});

describe("loader", () => {
  let restoreFetch: (() => void) | undefined;

  afterEach(() => {
    restoreFetch?.();
    restoreFetch = undefined;
  });

  // Bun's Request constructor needs a URL; tests use a relative path
  // because the loader only reads `request?.signal`.
  const dummyRequest = { signal: undefined } as any;
  const dummyParams = { id: "01ARZ3NDEKTSV4RRFFQ69G5FAV" };

  test("200 OK returns { data, error: null } with parsed envelope", async () => {
    const envelope = {
      data: [],
      meta: { truncated: false, total_changed: 0 },
    };
    restoreFetch = stubFetchOnce({
      status: 200,
      body:   JSON.stringify(envelope),
    });
    const result = await loader({ request: dummyRequest, params: dummyParams });
    expect(result.error).toBeNull();
    expect(result.data).toEqual(envelope);
  });

  test("404 returns empty-envelope signal { data: null, error: null }", async () => {
    restoreFetch = stubFetchOnce({ status: 404 });
    const result = await loader({ request: dummyRequest, params: dummyParams });
    expect(result.data).toBeNull();
    expect(result.error).toBeNull();
  });

  test("501 returns empty-envelope signal { data: null, error: null }", async () => {
    restoreFetch = stubFetchOnce({ status: 501 });
    const result = await loader({ request: dummyRequest, params: dummyParams });
    expect(result.data).toBeNull();
    expect(result.error).toBeNull();
  });

  test("500 populates error.requestId from the uniform error envelope", async () => {
    restoreFetch = stubFetchOnce({
      status: 500,
      body:   JSON.stringify({
        errors: [
          {
            status:     "500",
            title:      "Internal Server Error",
            detail:     "Run files materialization panicked.",
            request_id: "req_deadbeef",
          },
        ],
      }),
    });
    const result = await loader({ request: dummyRequest, params: dummyParams });
    expect(result.data).toBeNull();
    expect(result.error).not.toBeNull();
    expect(result.error!.status).toBe(500);
    expect(result.error!.requestId).toBe("req_deadbeef");
  });

  test("500 with no request_id leaves error.requestId as null", async () => {
    restoreFetch = stubFetchOnce({
      status: 500,
      body:   JSON.stringify({
        errors: [{ status: "500", title: "Internal", detail: "whoops" }],
      }),
    });
    const result = await loader({ request: dummyRequest, params: dummyParams });
    expect(result.error).not.toBeNull();
    expect(result.error!.status).toBe(500);
    expect(result.error!.requestId).toBeNull();
  });

  test("500 with non-JSON body still surfaces the status", async () => {
    restoreFetch = stubFetchOnce({ status: 500, body: "<html>oops</html>" });
    const result = await loader({ request: dummyRequest, params: dummyParams });
    expect(result.error).not.toBeNull();
    expect(result.error!.status).toBe(500);
    expect(result.error!.requestId).toBeNull();
  });

  test("503 populates error without requestId", async () => {
    restoreFetch = stubFetchOnce({
      status: 503,
      body:   JSON.stringify({ errors: [{ detail: "rate limited" }] }),
    });
    const result = await loader({ request: dummyRequest, params: dummyParams });
    expect(result.error).not.toBeNull();
    expect(result.error!.status).toBe(503);
  });

  test("401 still surfaces as an error (no in-loader redirect)", async () => {
    restoreFetch = stubFetchOnce({ status: 401 });
    const result = await loader({ request: dummyRequest, params: dummyParams });
    expect(result.error).not.toBeNull();
    expect(result.error!.status).toBe(401);
  });
});
