import { describe, expect, test } from "bun:test";
import { columnStatusDisplay, isRunStatus, mapRunSummaryToRunItem, runStatusDisplay } from "./runs";

describe("mapRunSummaryToRunItem", () => {
  test("maps store run summary to RunItem", () => {
    const summary = {
      run_id: "01ABC",
      goal: "Fix the build",
      workflow_slug: "fix_build",
      workflow_name: "Fix Build",
      host_repo_path: "/home/user/myrepo",
      status: "running",
      duration_ms: 65000,
      total_usd_micros: 500000,
      labels: {},
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
  });

  test("handles missing optional fields", () => {
    const summary = {
      run_id: "01DEF",
      goal: null,
      workflow_slug: null,
      workflow_name: null,
      host_repo_path: null,
      status: "submitted",
      duration_ms: null,
      total_usd_micros: null,
      labels: {},
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

  test("recognizes canonical blocked and queued run statuses", () => {
    expect(isRunStatus("queued")).toBe(true);
    expect(isRunStatus("blocked")).toBe(true);
    expect(runStatusDisplay).toHaveProperty("queued");
    expect(runStatusDisplay).toHaveProperty("blocked");
  });

  test("uses blocked board column instead of waiting", () => {
    expect(columnStatusDisplay).toHaveProperty("blocked");
    expect(columnStatusDisplay).not.toHaveProperty("waiting");
  });
});
