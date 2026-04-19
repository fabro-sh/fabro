import { describe, expect, test } from "bun:test";
import { columnForStatus, mapRunListItem, mapRunSummaryToRunItem } from "./runs";

describe("mapRunListItem", () => {
  test("trusts shared server fields for board items", () => {
    const summary = {
      run_id: "01ABC",
      goal: "## Fix the build",
      title: "Server supplied title",
      workflow_slug: "fix_build",
      workflow_name: "Fix Build",
      host_repo_path: "/home/user/myrepo",
      repository: { name: "myrepo" },
      status: "paused",
      labels: {},
      column: "running",
      elapsed_secs: 65,
      duration_ms: 65000,
      total_usd_micros: 500000,
      created_at: "2026-04-08T12:00:00Z",
      start_time: "2026-04-08T12:00:00Z",
      status_reason: null,
      pending_control: null,
    } as const;
    const item = mapRunListItem(summary);
    expect(item.id).toBe("01ABC");
    expect(item.title).toBe("Server supplied title");
    expect(item.workflow).toBe("fix_build");
    expect(item.repo).toBe("myrepo");
    expect(item.elapsed).toBeDefined();
    expect(item.column).toBe("running");
    expect(item.lifecycleStatus).toBe("paused");
  });

  test("uses a fallback title when the server title is blank", () => {
    const summary = {
      run_id: "01EMPTY",
      goal: "",
      title: "",
      workflow_slug: "fix_build",
      workflow_name: "Fix Build",
      host_repo_path: "/home/user/myrepo",
      repository: { name: "myrepo" },
      status: "running",
      labels: {},
      column: "running",
      elapsed_secs: null,
      duration_ms: null,
      total_usd_micros: null,
      created_at: "2026-04-08T12:00:00Z",
      start_time: null,
      status_reason: null,
      pending_control: null,
    } as const;

    expect(mapRunListItem(summary).title).toBe("Untitled run");
  });
});

describe("mapRunSummaryToRunItem", () => {
  test("maps canonical run summary to RunItem", () => {
    const summary = {
      run_id: "01ABC",
      goal: "Fix the build",
      title: "Fix the build",
      workflow_slug: "fix_build",
      workflow_name: "Fix Build",
      host_repo_path: "/home/user/myrepo",
      repository: { name: "myrepo" },
      status: "running",
      duration_ms: 65000,
      elapsed_secs: 65,
      total_usd_micros: 500000,
      labels: {},
      created_at: "2026-04-08T12:00:00Z",
      start_time: "2026-04-08T12:00:00Z",
      status_reason: null,
      pending_control: null,
    };
    const item = mapRunSummaryToRunItem(summary);
    expect(item.id).toBe("01ABC");
    expect(item.title).toBe("Fix the build");
    expect(item.workflow).toBe("fix_build");
    expect(item.repo).toBe("myrepo");
    expect(item.elapsed).toBeDefined();
    expect(item.lifecycleStatus).toBe("running");
  });

  test("handles missing optional fields", () => {
    const summary = {
      run_id: "01DEF",
      goal: "",
      title: "",
      workflow_slug: null,
      workflow_name: null,
      host_repo_path: null,
      repository: { name: "unknown" },
      status: "submitted",
      duration_ms: null,
      elapsed_secs: null,
      total_usd_micros: null,
      labels: {},
      created_at: "2026-04-08T12:00:00Z",
      start_time: null,
      status_reason: null,
      pending_control: null,
    };
    const item = mapRunSummaryToRunItem(summary);
    expect(item.id).toBe("01DEF");
    expect(item.title).toBe("Untitled run");
    expect(item.workflow).toBe("unknown");
    expect(item.repo).toBe("unknown");
  });
});

describe("columnForStatus", () => {
  test("returns null for lifecycle states that do not map to a board column", () => {
    expect(columnForStatus("removing")).toBeNull();
  });
});
