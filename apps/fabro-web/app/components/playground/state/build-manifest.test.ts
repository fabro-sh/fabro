import { describe, expect, test } from "bun:test";

import { createInitialDraft } from "./draft";
import { buildRunManifest, resolveWorkflowName } from "./build-manifest";

describe("resolveWorkflowName", () => {
  test("uses the draft name when set and valid", () => {
    const draft = { ...createInitialDraft(), name: "release_notes" };
    expect(resolveWorkflowName(draft)).toBe("release_notes");
  });

  test("falls back when the draft is still the default 'untitled'", () => {
    expect(resolveWorkflowName(createInitialDraft())).toBe(
      "playground_workflow",
    );
  });

  test("falls back for invalid names (snake_case rule)", () => {
    const draft = { ...createInitialDraft(), name: "Bad-Name!" };
    expect(resolveWorkflowName(draft)).toBe("playground_workflow");
  });
});

describe("buildRunManifest", () => {
  test("welcome draft → minimal manifest with inline DOT + TOML", () => {
    const manifest = buildRunManifest(createInitialDraft());
    expect(manifest.version).toBe(1);
    expect(manifest.target.identifier).toBe("playground_workflow");
    expect(manifest.target.path).toBe(
      ".fabro/workflows/playground_workflow/workflow.fabro",
    );
    const workflow =
      manifest.workflows[".fabro/workflows/playground_workflow/workflow.fabro"];
    expect(workflow).toBeDefined();
    expect(workflow!.source).toContain("digraph");
    expect(workflow!.source).toContain("start ->");
    expect(workflow!.config?.path).toBe("workflow.toml");
    expect(workflow!.config?.source).toContain("[run.sandbox]");
  });

  test("named draft → title and identifier use the snake_case name", () => {
    const draft = {
      ...createInitialDraft(),
      name: "release_notes",
      goal: "Generate release notes.",
    };
    const manifest = buildRunManifest(draft);
    expect(manifest.target.identifier).toBe("release_notes");
    expect(manifest.target.path).toBe(
      ".fabro/workflows/release_notes/workflow.fabro",
    );
    expect(manifest.title).toBe("Generate release notes.");
    expect(manifest.cwd).toBe("/tmp/fabro-playground");
  });

  test("title falls back when goal is empty", () => {
    const draft = { ...createInitialDraft(), name: "release_notes" };
    const manifest = buildRunManifest(draft);
    expect(manifest.title).toBe("Playground: release_notes");
  });
});
