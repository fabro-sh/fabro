import { describe, expect, test } from "bun:test";

import { createInitialDraft } from "../state/draft";
import { applyToolCalls } from "../state/reducer";
import { renderReadme } from "./render-readme";

describe("renderReadme", () => {
  test("uses fallback name and omits goal block for the welcome state", () => {
    const md = renderReadme(createInitialDraft());
    expect(md).toContain("# Playground workflow");
    expect(md).toContain("fabro run playground-workflow");
    expect(md).not.toContain(">"); // no goal line
  });

  test("renders the goal as a markdown quote when present", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "set_workflow_meta",
        args: { name: "release_notes", goal: "Generate release notes." },
      },
    ]);
    const md = renderReadme(draft);
    expect(md).toContain("# Release notes");
    expect(md).toContain("> Generate release notes.");
    expect(md).toContain("fabro run release_notes");
  });

  test("includes the Fabro blurb and learn-more link", () => {
    const md = renderReadme(createInitialDraft());
    expect(md).toContain("Fabro turns a Graphviz file");
    expect(md).toContain("https://fabro.sh");
  });
});
