import { describe, expect, test } from "bun:test";
import type { BoardColumn, Run } from "@qltysh/fabro-api-client";

import {
  buildBoardColumns,
  placeArchivedColumnLast,
  runsQuickStartCommands,
  shouldRefreshBoardForEvent,
} from "./runs";

function boardRun(id: string, column: BoardColumn, questionText?: string): Run {
  const status =
    column === "blocked"
      ? { kind: "blocked" as const, reason: "interview", pending_question_id: null }
      : column === "succeeded"
        ? { kind: "succeeded" as const, reason: "completed" }
        : column === "failed"
          ? { kind: "failed" as const, reason: "error" }
          : column === "queued"
            ? { kind: "queued" as const }
            : column === "initializing"
              ? { kind: "starting" as const }
              : { kind: "running" as const };
  return {
    id,
    goal:             `Run ${id}`,
    title:            `Run ${id}`,
    workflow:         { slug: "test", name: "Test" },
    automation:       null,
    repository:       { name: "repo", origin_url: null, provider: "unknown" },
    created_by:       null,
    origin:           { kind: "api" },
    labels:           {},
    lifecycle:        {
      status,
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
      "queued",
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
      "queued",
      "initializing",
      "running",
      "blocked",
      "succeeded",
      "failed",
      "archived",
    ]);
  });

  test("refreshes for blocked status and interview events", () => {
    expect(shouldRefreshBoardForEvent("run.queued")).toBe(true);
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
});
