import { describe, expect, test } from "bun:test";

import { createInitialDraft } from "../state/draft";
import { renderProjectToml, renderWorkflowToml } from "./render-toml";

describe("renderWorkflowToml", () => {
  test("points the workflow at its graph without an unsupported [run.sandbox] section", () => {
    // The server's RunLayer parses workflow.toml with deny_unknown_fields and
    // has no `sandbox` field, so a `[run.sandbox]` section makes the manifest
    // unparseable. Sandbox selection lives in project.toml instead.
    expect(renderWorkflowToml(createInitialDraft())).toBe(
      [
        "_version = 1",
        "",
        "[workflow]",
        'graph = "workflow.fabro"',
        "",
      ].join("\n"),
    );
  });
});

describe("renderProjectToml", () => {
  test("enables draft PRs and pins the default environment to the local sandbox", () => {
    expect(renderProjectToml(createInitialDraft())).toBe(
      [
        "_version = 1",
        "",
        "[run.pull_request]",
        "enabled = true",
        "draft   = true",
        "",
        "[environments.default]",
        'provider = "local"',
        "",
      ].join("\n"),
    );
  });
});
