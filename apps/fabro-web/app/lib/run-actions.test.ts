import { afterEach, describe, expect, test } from "bun:test";

import {
  archiveRun,
  canArchive,
  canCancel,
  canUnarchive,
  cancelRun,
  isTerminalCancelledRun,
  mapError,
  unarchiveRun,
} from "./run-actions";

type StubResponseInit = {
  status: number;
  body?: string;
  statusText?: string;
};

function stubFetchOnce(init: StubResponseInit) {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = (() =>
    Promise.resolve(
      new Response(init.body ?? "", {
        status: init.status,
        statusText: init.statusText ?? "",
        headers: { "Content-Type": "application/json" },
      }),
    )) as typeof fetch;

  return () => {
    globalThis.fetch = originalFetch;
  };
}

async function expectLifecycleError(
  input: Promise<unknown>,
): Promise<{ status: number; errors: Array<{ status: string; title: string; detail: string }> }> {
  try {
    await input;
    throw new Error("expected promise to reject");
  } catch (error) {
    return error as { status: number; errors: Array<{ status: string; title: string; detail: string }> };
  }
}

describe("run lifecycle actions", () => {
  let restoreFetch: (() => void) | undefined;

  afterEach(() => {
    restoreFetch?.();
    restoreFetch = undefined;
    delete (globalThis as { window?: unknown }).window;
  });

  test("cancelRun parses a 200 response", async () => {
    restoreFetch = stubFetchOnce({
      status: 200,
      body: JSON.stringify({
        id: "run-1",
        status: "failed",
        status_reason: "cancelled",
        created_at: "2026-04-20T12:00:00Z",
      }),
    });

    const result = await cancelRun("run-1");
    expect(result.status).toBe("failed");
    expect(result.status_reason).toBe("cancelled");
  });

  test("archiveRun parses a 200 response", async () => {
    restoreFetch = stubFetchOnce({
      status: 200,
      body: JSON.stringify({
        id: "run-1",
        status: "archived",
        created_at: "2026-04-20T12:00:00Z",
      }),
    });

    const result = await archiveRun("run-1");
    expect(result.status).toBe("archived");
  });

  test("unarchiveRun parses a 200 response", async () => {
    restoreFetch = stubFetchOnce({
      status: 200,
      body: JSON.stringify({
        id: "run-1",
        status: "succeeded",
        created_at: "2026-04-20T12:00:00Z",
      }),
    });

    const result = await unarchiveRun("run-1");
    expect(result.status).toBe("succeeded");
  });

  test("404 and 409 preserve the parsed error envelope", async () => {
    restoreFetch = stubFetchOnce({
      status: 404,
      body: JSON.stringify({
        errors: [{ status: "404", title: "Not Found", detail: "Run not found." }],
      }),
    });
    const notFound = await expectLifecycleError(cancelRun("missing-run"));
    expect(notFound).toEqual({
      status: 404,
      errors: [{ status: "404", title: "Not Found", detail: "Run not found." }],
    });

    restoreFetch = stubFetchOnce({
      status: 409,
      body: JSON.stringify({
        errors: [{ status: "409", title: "Conflict", detail: "Run is not terminal." }],
      }),
    });
    const conflict = await expectLifecycleError(archiveRun("run-1"));
    expect(conflict).toEqual({
      status: 409,
      errors: [{ status: "409", title: "Conflict", detail: "Run is not terminal." }],
    });
  });

  test("non-JSON error bodies fall back to an empty error list", async () => {
    restoreFetch = stubFetchOnce({
      status: 409,
      body: "<html>conflict</html>",
      statusText: "Conflict",
    });

    const error = await expectLifecycleError(unarchiveRun("run-1"));
    expect(error).toEqual({ status: 409, errors: [] });
  });

  test("mapError returns user-facing copy for lifecycle conflicts", () => {
    expect(mapError({ status: 409, errors: [] }, "cancel")).toBe("This run can no longer be cancelled.");
    expect(mapError({ status: 409, errors: [] }, "archive")).toBe("Only terminal runs can be archived.");
    expect(mapError({ status: 409, errors: [] }, "unarchive")).toBe("Active runs can't be unarchived.");
  });

  test("status predicates align with the documented run statuses", () => {
    expect(canCancel("submitted")).toBe(true);
    expect(canCancel("queued")).toBe(true);
    expect(canCancel("starting")).toBe(true);
    expect(canCancel("running")).toBe(true);
    expect(canCancel("paused")).toBe(true);
    expect(canCancel("blocked")).toBe(false);
    expect(canCancel("archived")).toBe(false);

    expect(canArchive("succeeded")).toBe(true);
    expect(canArchive("failed")).toBe(true);
    expect(canArchive("dead")).toBe(true);
    expect(canArchive("archived")).toBe(false);

    expect(canUnarchive("archived")).toBe(true);
    expect(canUnarchive("failed")).toBe(false);
  });

  test("isTerminalCancelledRun distinguishes immediate cancel success from in-flight cancellation", () => {
    expect(
      isTerminalCancelledRun({
        id: "run-1",
        status: "failed",
        status_reason: "cancelled",
        created_at: "2026-04-20T12:00:00Z",
      }),
    ).toBe(true);
    expect(
      isTerminalCancelledRun({
        id: "run-1",
        status: "running",
        pending_control: "cancel",
        created_at: "2026-04-20T12:00:00Z",
      }),
    ).toBe(false);
  });
});
