import { describe, expect, test } from "bun:test";
import type { Run, RunStatus as ApiRunStatus } from "@qltysh/fabro-api-client";
import {
  columnForStatus,
  columnStatusDisplay,
  isRunStatus,
  mapRunListItem,
  mapRunToRunItem,
  runStatusDisplay,
} from "./runs";

function makeRun(overrides: Partial<Run> = {}): Run {
  return {
    id:               "01ABC",
    goal:             "Fix the build",
    title:            "Fix the build",
    workflow:         { slug: "fix_build", name: "Fix Build" },
    automation:       null,
    repository:       { name: "myrepo", origin_url: null, provider: "unknown" },
    created_by:       null,
    origin:           { kind: "api" },
    labels:           {},
    lifecycle:        {
      status:          { kind: "running" },
      pending_control: null,
      queue_position:  null,
      error:           null,
      archived:        false,
      archived_at:     null,
    },
    sandbox:          null,
    models:           [],
    source_directory: "/home/user/myrepo",
    timestamps:       {
      created_at:     "2026-04-08T12:00:00Z",
      started_at:     "2026-04-08T12:00:00Z",
      last_event_at:  null,
      completed_at:   null,
      duration_ms:    65000,
      elapsed_secs:   65,
    },
    billing:          { total_usd_micros: 500000 },
    diff:             null,
    pull_request:     null,
    current_question: null,
    superseded_by:    null,
    links:            { web: null },
    ...overrides,
  };
}

function withStatus(status: ApiRunStatus): Pick<Run, "lifecycle"> {
  return {
    lifecycle: {
      status,
      pending_control: null,
      queue_position:  null,
      error:           null,
      archived:        false,
      archived_at:     null,
    },
  };
}

describe("mapRunListItem", () => {
  test("trusts shared server fields for board items", () => {
    const summary = makeRun({
      title:         "Server supplied title",
      ...withStatus({ kind: "paused", prior_block: null }),
      pull_request: {
        owner: "fabro-sh",
        repo: "fabro",
        number: 123,
        html_url: "https://github.com/fabro-sh/fabro/pull/123",
      },
    });
    const item = mapRunListItem(summary);
    expect(item.id).toBe("01ABC");
    expect(item.title).toBe("Server supplied title");
    expect(item.workflow).toBe("fix_build");
    expect(item.repo).toBe("myrepo");
    expect(item.sourceDirectory).toBe("/home/user/myrepo");
    expect(item.elapsed).toBeDefined();
    expect(item.column).toBe("running");
    expect(item.lifecycleStatus).toBe("paused");
    expect(item.number).toBe(123);
    expect(item.pullRequestUrl).toBe("https://github.com/fabro-sh/fabro/pull/123");
  });

  test("uses a fallback title when the server title is blank", () => {
    const summary = makeRun({ id: "01EMPTY", goal: "", title: "" });

    expect(mapRunListItem(summary).title).toBe("Untitled run");
  });
});

describe("mapRunToRunItem", () => {
  test("maps canonical run summary to RunItem", () => {
    const summary = makeRun({
      pull_request: {
        owner: "fabro-sh",
        repo: "fabro",
        number: 456,
        html_url: "https://github.com/fabro-sh/fabro/pull/456",
      },
    });
    const item = mapRunToRunItem(summary);
    expect(item.id).toBe("01ABC");
    expect(item.title).toBe("Fix the build");
    expect(item.workflow).toBe("fix_build");
    expect(item.repo).toBe("myrepo");
    expect(item.sourceDirectory).toBe("/home/user/myrepo");
    expect(item.elapsed).toBeDefined();
    expect(item.lifecycleStatus).toBe("running");
    expect(item.number).toBe(456);
    expect(item.pullRequestUrl).toBe("https://github.com/fabro-sh/fabro/pull/456");
  });

  test("handles missing optional fields", () => {
    const summary = makeRun({
      id:               "01DEF",
      goal:             "",
      title:            "",
      workflow:         { slug: null, name: "unknown" },
      source_directory: null,
      repository:       { name: "unknown", origin_url: null, provider: "unknown" },
      ...withStatus({ kind: "submitted" }),
      timestamps:       {
        created_at:     "2026-04-08T12:00:00Z",
        started_at:     null,
        last_event_at:  null,
        completed_at:   null,
        duration_ms:    null,
        elapsed_secs:   null,
      },
      billing:          null,
    });
    const item = mapRunToRunItem(summary);
    expect(item.id).toBe("01DEF");
    expect(item.title).toBe("Untitled run");
    expect(item.workflow).toBe("unknown");
    expect(item.repo).toBe("unknown");
    expect(item.sourceDirectory).toBeUndefined();
  });

  test("recognizes canonical blocked and queued run statuses", () => {
    expect(isRunStatus("queued")).toBe(true);
    expect(isRunStatus("blocked")).toBe(true);
    expect(runStatusDisplay).toHaveProperty("queued");
    expect(runStatusDisplay).toHaveProperty("blocked");
  });

  test("recognizes archived as a terminal run status", () => {
    expect(isRunStatus("archived")).toBe(true);
    expect(runStatusDisplay).toHaveProperty("archived");
  });

  test("uses blocked board column instead of waiting", () => {
    expect(columnStatusDisplay).toHaveProperty("blocked");
    expect(columnStatusDisplay).not.toHaveProperty("waiting");
  });
});

describe("columnForStatus", () => {
  test("returns null for lifecycle states that do not map to a board column", () => {
    expect(columnForStatus("removing")).toBeNull();
  });
});
