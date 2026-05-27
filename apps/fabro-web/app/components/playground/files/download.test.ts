import { describe, expect, test } from "bun:test";
import { unzipSync, strFromU8 } from "fflate";

import { createInitialDraft } from "../state/draft";
import { applyToolCalls } from "../state/reducer";
import { buildDownloadBundle, resolveWorkflowName } from "./download";

describe("resolveWorkflowName", () => {
  test("returns the fallback when the draft is still 'untitled'", () => {
    expect(resolveWorkflowName(createInitialDraft())).toBe(
      "playground-workflow",
    );
  });

  test("returns the draft name when it's valid snake_case", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      { name: "set_workflow_meta", args: { name: "release_notes" } },
    ]);
    expect(resolveWorkflowName(draft)).toBe("release_notes");
  });
});

describe("buildDownloadBundle", () => {
  test("welcome state still produces a runnable artifact", () => {
    const bundle = buildDownloadBundle(createInitialDraft());
    expect(bundle.workflowName).toBe("playground-workflow");
    expect(bundle.zipFilename).toBe("playground-workflow.fabro.zip");
    expect(bundle.bytes.length).toBeGreaterThan(0);

    const entries = unzipSync(bundle.bytes);
    expect(
      strFromU8(
        entries[".fabro/workflows/playground-workflow/workflow.fabro"]!,
      ),
    ).toContain("digraph");
    expect(
      strFromU8(
        entries[".fabro/workflows/playground-workflow/workflow.toml"]!,
      ),
    ).toContain('graph = "workflow.fabro"');
    expect(strFromU8(entries[".fabro/project.toml"]!)).toContain(
      "[run.pull_request]",
    );
    expect(strFromU8(entries["README.md"]!)).toContain(
      "fabro run playground-workflow",
    );
  });

  test("named workflow zip layout uses the snake_case name", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      { name: "set_workflow_meta", args: { name: "release_notes" } },
      {
        name: "add_node",
        args: { id: "plan", label: "Plan", shape: "box", prompt: "Plan it" },
      },
    ]);
    const bundle = buildDownloadBundle(draft);
    expect(bundle.workflowName).toBe("release_notes");
    expect(bundle.zipFilename).toBe("release_notes.fabro.zip");

    const entries = unzipSync(bundle.bytes);
    const expectedPaths = [
      ".fabro/project.toml",
      ".fabro/workflows/release_notes/workflow.fabro",
      ".fabro/workflows/release_notes/workflow.toml",
      "README.md",
    ];
    for (const path of expectedPaths) {
      expect(entries[path]).toBeDefined();
    }

    expect(
      strFromU8(entries[".fabro/workflows/release_notes/workflow.fabro"]!),
    ).toContain("plan [shape=box, label=\"Plan\", prompt=\"Plan it\"]");
  });
});
