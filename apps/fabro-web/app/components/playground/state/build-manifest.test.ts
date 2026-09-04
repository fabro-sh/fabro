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
    expect(manifest.target).toEqual({
      path: ".fabro/workflows/playground_workflow/workflow.fabro",
    });
    const workflow =
      manifest.workflows[".fabro/workflows/playground_workflow/workflow.fabro"];
    expect(workflow).toBeDefined();
    expect(workflow!.source).toContain("digraph");
    expect(workflow!.source).toContain("start ->");
    expect(workflow!.config?.path).toBe("workflow.toml");
    // workflow.toml must stay parseable by the server's RunLayer, which
    // rejects the unknown `[run.sandbox]` section.
    expect(workflow!.config?.source).not.toContain("[run.sandbox]");
    expect(workflow!.config?.source).toContain("[workflow]");
  });

  test("clamps an over-long goal so the title stays within the server's 100-char limit", () => {
    const goal = "a".repeat(150);
    const draft = { ...createInitialDraft(), name: "release_notes", goal };
    const manifest = buildRunManifest(draft);
    expect(manifest.title).toBeDefined();
    // Server caps RunManifest.title at 100 Unicode scalar values; count by
    // code point (not UTF-16 units) to match its `chars().count()` check.
    expect(Array.from(manifest.title!).length).toBeLessThanOrEqual(100);
    expect(manifest.title!.endsWith("…")).toBe(true);
  });

  test("named draft → title and target path use the snake_case name", () => {
    const draft = {
      ...createInitialDraft(),
      name: "release_notes",
      goal: "Generate release notes.",
    };
    const manifest = buildRunManifest(draft);
    expect(manifest.target).toEqual({
      path: ".fabro/workflows/release_notes/workflow.fabro",
    });
    expect(manifest.title).toBe("Generate release notes.");
    expect(manifest.cwd).toBe("/tmp/fabro-playground");
  });

  test("title falls back when goal is empty", () => {
    const draft = { ...createInitialDraft(), name: "release_notes" };
    const manifest = buildRunManifest(draft);
    expect(manifest.title).toBe("Playground: release_notes");
  });
});
