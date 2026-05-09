import { afterEach, describe, expect, test } from "bun:test";
import type { AxiosAdapter } from "axios";

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
import { generatedAxios } from "./api-client";

type StubResponseInit = {
  status: number;
  body?: unknown;
  statusText?: string;
};

const originalAdapter = generatedAxios.defaults.adapter;

function stubGeneratedAxiosOnce(init: StubResponseInit) {
  generatedAxios.defaults.adapter = (async (config) => {
    if (init.status >= 400) {
      throw {
        isAxiosError: true,
        message: init.statusText ?? `HTTP ${init.status}`,
        response: {
          status: init.status,
          statusText: init.statusText ?? "",
          data: init.body ?? null,
          headers: {},
        },
      };
    }
    return {
      data: init.body,
      status: init.status,
      statusText: init.statusText ?? "",
      headers: {},
      config,
    };
  }) as AxiosAdapter;
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
  afterEach(() => {
    generatedAxios.defaults.adapter = originalAdapter;
    delete (globalThis as { window?: unknown }).window;
  });

  test("cancelRun parses a 200 response", async () => {
    stubGeneratedAxiosOnce({
      status: 200,
      body: {
        id: "run-1",
        status: { kind: "failed", reason: "cancelled" },
        created_at: "2026-04-20T12:00:00Z",
      },
    });

    const result = await cancelRun("run-1");
    expect(result.status.kind).toBe("failed");
    if (result.status.kind === "failed") {
      expect(result.status.reason).toBe("cancelled");
    }
  });

  test("archiveRun parses a 200 response", async () => {
    stubGeneratedAxiosOnce({
      status: 200,
      body: {
        id: "run-1",
        status: {
          kind: "archived",
          prior: { kind: "succeeded", reason: "completed" },
        },
        created_at: "2026-04-20T12:00:00Z",
      },
    });

    const result = await archiveRun("run-1");
    expect(result.status.kind).toBe("archived");
  });

  test("unarchiveRun parses a 200 response", async () => {
    stubGeneratedAxiosOnce({
      status: 200,
      body: {
        id: "run-1",
        status: { kind: "succeeded", reason: "completed" },
        created_at: "2026-04-20T12:00:00Z",
      },
    });

    const result = await unarchiveRun("run-1");
    expect(result.status.kind).toBe("succeeded");
  });

  test("404 and 409 preserve the parsed error envelope", async () => {
    stubGeneratedAxiosOnce({
      status: 404,
      body: {
        errors: [{ status: "404", title: "Not Found", detail: "Run not found." }],
      },
    });
    const notFound = await expectLifecycleError(cancelRun("missing-run"));
    expect(notFound).toEqual({
      status: 404,
      errors: [{ status: "404", title: "Not Found", detail: "Run not found." }],
    });

    stubGeneratedAxiosOnce({
      status: 409,
      body: {
        errors: [{ status: "409", title: "Conflict", detail: "Run is not terminal." }],
      },
    });
    const conflict = await expectLifecycleError(archiveRun("run-1"));
    expect(conflict).toEqual({
      status: 409,
      errors: [{ status: "409", title: "Conflict", detail: "Run is not terminal." }],
    });
  });

  test("non-JSON error bodies fall back to an empty error list", async () => {
    stubGeneratedAxiosOnce({
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
    expect(canCancel("blocked")).toBe(true);
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
        status: { kind: "failed", reason: "cancelled" },
        created_at: "2026-04-20T12:00:00Z",
      }),
    ).toBe(true);
    expect(
      isTerminalCancelledRun({
        id: "run-1",
        status: { kind: "running" },
        pending_control: "cancel",
        created_at: "2026-04-20T12:00:00Z",
      }),
    ).toBe(false);
  });
});
