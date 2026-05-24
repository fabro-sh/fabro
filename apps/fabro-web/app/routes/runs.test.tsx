import { describe, expect, test } from "bun:test";
import type { BoardColumn, Run } from "@qltysh/fabro-api-client";

import {
  buildBoardColumns,
  loadStoredRunsWorkspaceSearchParams,
  placeArchivedColumnLast,
  persistRunsWorkspaceSearchParams,
  RUNS_PREFERENCES_STORAGE_KEY,
  runsQuickStartCommands,
  shouldRefreshBoardForEvent,
  summarizeBatchLifecycleAction,
} from "./runs";

function boardRun(id: string, column: BoardColumn, questionText?: string): Run {
  const status =
    column === "blocked"
      ? { kind: "blocked" as const, blocked_reason: "human_input_required" as const }
      : column === "succeeded"
        ? { kind: "succeeded" as const, reason: "completed" }
      : column === "failed"
        ? { kind: "failed" as const, reason: "workflow_error" as const }
        : column === "pending"
          ? { kind: "pending" as const, reason: "approval_required" as const }
          : column === "runnable"
            ? { kind: "runnable" as const }
            : column === "initializing"
              ? { kind: "starting" as const }
              : { kind: "running" as const };
  return {
    id,
    goal:             `Run ${id}`,
    title:            `Run ${id}`,
    workflow:         { slug: "test", name: "Test", graph_name: null, node_count: 0, edge_count: 0 },
    automation:       null,
    repository:       { name: "repo", origin_url: null, provider: "unknown" },
    created_by:       null,
    origin:           { kind: "api" },
    labels:           {},
    lifecycle:        {
      status,
      approval: null,
      pending_control: null,
      queue_position:  null,
      error:           null,
      archived:        column === "archived",
      archived_at:     column === "archived" ? "2026-04-19T12:05:00Z" : null,
    },
    sandbox:          null,
    models:           [],
    source_directory: null,
    timestamps:       {
      created_at:     "2026-04-19T12:00:00Z",
      started_at:     null,
      last_event_at:  null,
      completed_at:   null,
    },
    billing:          null,
    size:             "XS",
    diff:             null,
    pull_request:     null,
    current_question: questionText ? { text: questionText } : null,
    superseded_by:    null,
    retried_from:     null,
    links:            { web: null },
  };
}

describe("runs route board mapping", () => {
  test("keeps blocked runs in the blocked lane and preserves question text", () => {
    const columns = buildBoardColumns(
      {
        data: [
          boardRun("paused-run", "running"),
          boardRun("blocked-run", "blocked", "Older unresolved question?"),
        ],
      },
      false,
    );

    expect(columns.find((column) => column.id === "running")?.items.map((item) => item.id)).toContain("paused-run");
    expect(columns.find((column) => column.id === "blocked")?.items.map((item) => item.id)).toContain("blocked-run");
    expect(columns.find((column) => column.id === "blocked")?.items[0]?.question).toBe("Older unresolved question?");
  });

  test("renders an archived column when includeArchived is true", () => {
    const columns = buildBoardColumns(
      {
        data: [
          boardRun("succeeded-run", "succeeded"),
          boardRun("archived-run", "archived"),
        ],
      },
      true,
    );

    expect(columns.map((column) => column.id)).toEqual([
      "pending",
      "runnable",
      "initializing",
      "running",
      "blocked",
      "succeeded",
      "failed",
      "archived",
    ]);
    expect(
      columns.find((column) => column.id === "archived")?.items.map((item) => item.id),
    ).toEqual(["archived-run"]);
    expect(
      columns.find((column) => column.id === "succeeded")?.items.map((item) => item.id),
    ).toEqual(["succeeded-run"]);
  });

  test("omits the archived column when includeArchived is false", () => {
    const columns = buildBoardColumns(
      { data: [boardRun("succeeded-run", "succeeded")] },
      false,
    );

    expect(columns.some((column) => column.id === "archived")).toBe(false);
  });

  test("places the archived column last when active", () => {
    const columns = buildBoardColumns(
      {
        data: [
          boardRun("running-run", "running"),
          boardRun("succeeded-run", "succeeded"),
          boardRun("archived-run", "archived"),
        ],
      },
      true,
    );

    expect(placeArchivedColumnLast(columns, true).map((column) => column.id)).toEqual([
      "pending",
      "runnable",
      "initializing",
      "running",
      "blocked",
      "succeeded",
      "failed",
      "archived",
    ]);
  });

  test("refreshes for blocked status and interview events", () => {
    expect(shouldRefreshBoardForEvent("run.pending")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.runnable")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.approved")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.denied")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.blocked")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.unblocked")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.archived")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.unarchived")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.title.updated")).toBe(true);
    expect(shouldRefreshBoardForEvent("interview.started")).toBe(true);
    expect(shouldRefreshBoardForEvent("interview.completed")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.created")).toBe(false);
  });

  test("includes the configured server argument for GitHub-auth quick starts", () => {
    expect(runsQuickStartCommands(true, "http://127.0.0.1:32276")).toEqual([
      "fabro auth login --server http://127.0.0.1:32276",
      "fabro repo init",
      "fabro run hello",
    ]);
  });

  test("does not show a placeholder server when system info is unavailable", () => {
    expect(runsQuickStartCommands(true)).toEqual([
      "fabro repo init",
      "fabro run hello",
    ]);
  });

  test("summarizes successful batch archive and unarchive actions", () => {
    expect(
      summarizeBatchLifecycleAction("Archive", { requested: 2, succeeded: 2, failed: 0 }),
    ).toEqual({ message: "Archived 2 runs." });
    expect(
      summarizeBatchLifecycleAction("Unarchive", { requested: 1, succeeded: 1, failed: 0 }),
    ).toEqual({ message: "Unarchived 1 run." });
  });

  test("summarizes partial and failed batch lifecycle actions", () => {
    expect(
      summarizeBatchLifecycleAction("Archive", { requested: 3, succeeded: 2, failed: 1 }),
    ).toEqual({
      message: "Archived 2 of 3 runs. 1 failed.",
      tone:    "error",
    });
    expect(
      summarizeBatchLifecycleAction("Unarchive", { requested: 2, succeeded: 0, failed: 2 }),
    ).toEqual({
      message: "Couldn't unarchive 2 runs. Try again.",
      tone:    "error",
    });
  });
});

describe("runs route workspace preferences", () => {
  class MemoryStorage {
    values = new Map<string, string>();

    getItem(key: string) {
      return this.values.get(key) ?? null;
    }

    setItem(key: string, value: string) {
      this.values.set(key, value);
    }
  }

  test("missing storage returns default search params", () => {
    expect(loadStoredRunsWorkspaceSearchParams(null).toString()).toBe("");
  });

  test("invalid stored values are ignored", () => {
    const storage = new MemoryStorage();
    storage.setItem(
      RUNS_PREFERENCES_STORAGE_KEY,
      JSON.stringify({
        version:   1,
        view:      "table",
        created:   "tomorrow",
        archived:  "yes",
        sort:      "branch",
        direction: "sideways",
        size:      999,
        hide:      "repo,unknown,elapsed",
        page:      12,
      }),
    );

    expect(loadStoredRunsWorkspaceSearchParams(storage).toString()).toBe("hide=elapsed%2Crepo");
  });

  test("valid stored preferences produce canonical URL params", () => {
    const storage = new MemoryStorage();
    storage.setItem(
      RUNS_PREFERENCES_STORAGE_KEY,
      JSON.stringify({
        version:   1,
        view:      "list",
        search:    "retry failures",
        repo:      "qlty/fabro",
        workflow:  "release",
        created:   "7d",
        archived:  true,
        sort:      "updated_at",
        direction: "asc",
        size:      50,
        hide:      "repo,changes",
        page:      4,
      }),
    );

    expect(loadStoredRunsWorkspaceSearchParams(storage).toString()).toBe(
      "view=list&search=retry+failures&repo=qlty%2Ffabro&workflow=release&created=7d&archived=1&sort=updated_at&direction=asc&size=50&hide=repo%2Cchanges",
    );
  });

  test("persisting preferences omits page and stores canonical values", () => {
    const storage = new MemoryStorage();
    const params = new URLSearchParams(
      "view=columns&search=abc&created=1d&sort=made-up&direction=asc&size=100&page=9&hide=unknown,workflow,repo",
    );

    persistRunsWorkspaceSearchParams(params, storage);

    expect(JSON.parse(storage.getItem(RUNS_PREFERENCES_STORAGE_KEY) ?? "{}")).toEqual({
      version:   1,
      view:      "columns",
      search:    "abc",
      repo:      "all",
      workflow:  "all",
      created:   "1d",
      archived:  false,
      sort:      "created_at",
      direction: "asc",
      size:      100,
      hide:      "repo,workflow",
    });
  });
});
