import { describe, expect, test } from "bun:test";

import { createInitialDraft } from "../state/draft";
import { renderProjectToml, renderWorkflowToml } from "./render-toml";

describe("renderWorkflowToml", () => {
  test("points the workflow at workflow.fabro and pins sandbox to local", () => {
    expect(renderWorkflowToml(createInitialDraft())).toBe(
      [
        "_version = 1",
        "",
        "[workflow]",
        'graph = "workflow.fabro"',
        "",
        "[run.sandbox]",
        'provider = "local"',
        "",
      ].join("\n"),
    );
  });
});

describe("renderProjectToml", () => {
  test("enables draft PRs by default", () => {
    expect(renderProjectToml(createInitialDraft())).toBe(
      [
        "_version = 1",
        "",
        "[run.pull_request]",
        "enabled = true",
        "draft   = true",
        "",
      ].join("\n"),
    );
  });
});
