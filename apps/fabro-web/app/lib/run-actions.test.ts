import { afterEach, describe, expect, test } from "bun:test";
import type { AxiosAdapter } from "axios";
import type { Run, RunStatus } from "@qltysh/fabro-api-client";

import {
  archiveRun,
  canArchive,
  canCancel,
  canRetry,
  canUnarchive,
  cancelRun,
  isTerminalCancelledRun,
  mapError,
  retryRun,
  unarchiveRun,
} from "./run-actions";
import { generatedAxios } from "./api-client";

type StubResponseInit = {
  status: number;
  body?: unknown;
  statusText?: string;
};

const originalAdapter = generatedAxios.defaults.adapter;

function makeRun(status: RunStatus, archived = false): Run {
  return {
    id:               "run-1",
    goal:             "Fix the build",
    title:            "Fix the build",
    workflow:         { slug: "fix_build", name: "Fix Build" },
    automation:       null,
    repository:       null,
    created_by:       null,
    origin:           { kind: "api" },
    labels:           {},
    lifecycle:        {
      status,
      pending_control: null,
      queue_position:  null,
      error:           null,
      archived,
      archived_at:     archived ? "2026-04-20T12:05:00Z" : null,
    },
    sandbox:          null,
    models:           [],
    source_directory: null,
    timestamps:       {
      created_at:     "2026-04-20T12:00:00Z",
      started_at:     null,
      last_event_at:  null,
      completed_at:   null,
    },
    billing:          null,
    size:             "XS",
    diff:             null,
    pull_request:     null,
    current_question: null,
    superseded_by:    null,
    retried_from:     null,
    links:            { web: null },
  };
}

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
      body: makeRun({ kind: "failed", reason: "cancelled" }),
    });

    const result = await cancelRun("run-1");
    expect(result.lifecycle.status.kind).toBe("failed");
    if (result.lifecycle.status.kind === "failed") {
      expect(result.lifecycle.status.reason).toBe("cancelled");
    }
  });

  test("archiveRun parses a 200 response", async () => {
    stubGeneratedAxiosOnce({
      status: 200,
      body: makeRun({ kind: "succeeded", reason: "completed" }, true),
    });

    const result = await archiveRun("run-1");
    expect(result.lifecycle.status.kind).toBe("succeeded");
    expect(result.lifecycle.archived).toBe(true);
  });

  test("unarchiveRun parses a 200 response", async () => {
    stubGeneratedAxiosOnce({
      status: 200,
      body: makeRun({ kind: "succeeded", reason: "completed" }),
    });

    const result = await unarchiveRun("run-1");
    expect(result.lifecycle.status.kind).toBe("succeeded");
    expect(result.lifecycle.archived).toBe(false);
  });

  test("retryRun parses a 201 response", async () => {
    stubGeneratedAxiosOnce({
      status: 201,
      body: {
        ...makeRun({ kind: "queued" }),
        id:           "run-2",
        retried_from: "run-1",
      },
    });

    const result = await retryRun("run-1");
    expect(result.id).toBe("run-2");
    expect(result.retried_from).toBe("run-1");
    expect(result.lifecycle.status.kind).toBe("queued");
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

  test("canRetry allows failed and dead runs except cancelled or archived runs", () => {
    expect(canRetry(makeRun({ kind: "failed", reason: "workflow_error" }))).toBe(true);
    expect(canRetry(makeRun({ kind: "dead" }))).toBe(true);
    expect(canRetry(makeRun({ kind: "failed", reason: "cancelled" }))).toBe(false);
    expect(canRetry(makeRun({ kind: "succeeded", reason: "completed" }))).toBe(false);
    expect(canRetry(makeRun({ kind: "running" }))).toBe(false);
    expect(canRetry(makeRun({ kind: "failed", reason: "workflow_error" }, true))).toBe(false);
  });

  test("isTerminalCancelledRun distinguishes immediate cancel success from in-flight cancellation", () => {
    expect(
      isTerminalCancelledRun(makeRun({ kind: "failed", reason: "cancelled" })),
    ).toBe(true);
    expect(
      isTerminalCancelledRun(
        makeRun({ kind: "running" }, false),
      ),
    ).toBe(false);
  });
});
