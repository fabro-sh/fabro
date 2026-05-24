import { afterEach, describe, expect, test } from "bun:test";
import type { AxiosAdapter } from "axios";
import type { BatchRunLifecycleResponse, Run, RunStatus } from "@qltysh/fabro-api-client";

import {
  archiveRun,
  archiveRuns,
  canArchive,
  canApprove,
  canCancel,
  canRetry,
  canUnarchive,
  cancelRun,
  isTerminalCancelledRun,
  mapError,
  retryRun,
  unarchiveRun,
  unarchiveRuns,
} from "./run-actions";
import { generatedAxios } from "./api-client";

type StubResponseInit = {
  status: number;
  body?: unknown;
  statusText?: string;
};

type CapturedRequest = {
  url?: string;
  method?: string;
  data?: unknown;
};

const originalAdapter = generatedAxios.defaults.adapter;

function makeRun(status: RunStatus, archived = false): Run {
  return {
    id:               "run-1",
    goal:             "Fix the build",
    title:            "Fix the build",
    workflow:         { slug: "fix_build", name: "Fix Build", graph_name: null, node_count: 0, edge_count: 0 },
    automation:       null,
    repository:       null,
    created_by:       null,
    origin:           { kind: "api" },
    labels:           {},
    lifecycle:        {
      status,
      approval: null,
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

function stubGeneratedAxiosOnce(init: StubResponseInit): { requests: CapturedRequest[] } {
  const requests: CapturedRequest[] = [];
  generatedAxios.defaults.adapter = (async (config) => {
    requests.push({
      url: config.url,
      method: config.method,
      data: config.data,
    });
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
  return { requests };
}

function batchResponse(
  results: BatchRunLifecycleResponse["results"],
): BatchRunLifecycleResponse {
  const succeeded = results.filter((result) => result.ok).length;
  return {
    results,
    summary: {
      requested: results.length,
      succeeded,
      failed: results.length - succeeded,
    },
  };
}

function requestJsonBody(request: CapturedRequest): unknown {
  return typeof request.data === "string" ? JSON.parse(request.data) : request.data;
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

  test("archiveRuns sends one batch request and parses results", async () => {
    const stub = stubGeneratedAxiosOnce({
      status: 200,
      body: batchResponse([
        {
          run_id: "run-1",
          ok: true,
          outcome: "archived",
          run: { ...makeRun({ kind: "succeeded", reason: "completed" }, true), id: "run-1" },
        },
        {
          run_id: "run-2",
          ok: true,
          outcome: "already_archived",
          run: { ...makeRun({ kind: "succeeded", reason: "completed" }, true), id: "run-2" },
        },
      ]),
    });

    const result = await archiveRuns(["run-1", "run-2"]);

    expect(stub.requests).toHaveLength(1);
    expect(stub.requests[0]?.method?.toUpperCase()).toBe("POST");
    expect(stub.requests[0]?.url).toBe("/api/v1/runs/archive");
    expect(requestJsonBody(stub.requests[0]!)).toEqual({ run_ids: ["run-1", "run-2"] });
    expect(result.summary).toEqual({ requested: 2, succeeded: 2, failed: 0 });
    expect(result.results.map((entry) => entry.outcome)).toEqual(["archived", "already_archived"]);
  });

  test("unarchiveRuns resolves mixed per-item results without throwing", async () => {
    stubGeneratedAxiosOnce({
      status: 200,
      body: batchResponse([
        {
          run_id: "run-1",
          ok: true,
          outcome: "unarchived",
          run: { ...makeRun({ kind: "succeeded", reason: "completed" }), id: "run-1" },
        },
        {
          run_id: "run-missing",
          ok: false,
          outcome: "not_found",
          error: { status: "404", title: "Not Found", detail: "Run not found." },
        },
      ]),
    });

    const result = await unarchiveRuns(["run-1", "run-missing"]);

    expect(result.summary).toEqual({ requested: 2, succeeded: 1, failed: 1 });
    expect(result.results[1]?.ok).toBe(false);
    expect(result.results[1]?.error?.status).toBe("404");
  });

  test("batch lifecycle helpers preserve request-level error envelopes", async () => {
    stubGeneratedAxiosOnce({
      status: 400,
      body: {
        errors: [{ status: "400", title: "Bad Request", detail: "run_ids must contain at least one run ID." }],
      },
    });

    const error = await expectLifecycleError(archiveRuns([]));
    expect(error).toEqual({
      status: 400,
      errors: [{ status: "400", title: "Bad Request", detail: "run_ids must contain at least one run ID." }],
    });
  });

  test("retryRun parses a 201 response", async () => {
    stubGeneratedAxiosOnce({
      status: 201,
      body: {
        ...makeRun({ kind: "submitted" }),
        id:           "run-2",
        retried_from: "run-1",
      },
    });

    const result = await retryRun("run-1");
    expect(result.id).toBe("run-2");
    expect(result.retried_from).toBe("run-1");
    expect(result.lifecycle.status.kind).toBe("submitted");
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
    expect(mapError({ status: 409, errors: [] }, "approve")).toBe("This run is no longer pending approval.");
    expect(mapError({ status: 409, errors: [] }, "deny")).toBe("This run is no longer pending approval.");
    expect(mapError({ status: 409, errors: [] }, "archive")).toBe("Only terminal runs can be archived.");
    expect(mapError({ status: 409, errors: [] }, "unarchive")).toBe("Active runs can't be unarchived.");
  });

  test("status predicates align with the documented run statuses", () => {
    expect(canCancel("submitted")).toBe(true);
    expect(canCancel("pending")).toBe(true);
    expect(canCancel("runnable")).toBe(true);
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

  test("approval predicate requires pending status and pending approval state", () => {
    expect(canApprove({
      ...makeRun({ kind: "pending", reason: "approval_required" }),
      lifecycle: {
        ...makeRun({ kind: "pending", reason: "approval_required" }).lifecycle,
        approval: {
          state: "pending",
          requested_at: "2026-05-23T12:00:00Z",
          decided_at: null,
          denial_reason: null,
        },
      },
    })).toBe(true);
    expect(canApprove(makeRun({ kind: "pending", reason: "approval_required" }))).toBe(false);
    expect(canApprove(makeRun({ kind: "runnable" }))).toBe(false);
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
