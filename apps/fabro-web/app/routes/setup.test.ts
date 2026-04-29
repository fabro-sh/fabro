import { describe, expect, test } from "bun:test";

import { setupContentForSearch } from "./setup";

describe("setupContentForSearch", () => {
  test("explains GitHub App installation returns separately from first-time setup", () => {
    const content = setupContentForSearch(
      "?installation_id=128003036&setup_action=install",
    );

    expect(content.title).toBe("GitHub App installed");
    expect(content.description).toContain(
      "GitHub finished installing the app",
    );
    expect(content.steps.map((step) => step.title)).toEqual([
      "Return to Fabro",
      "Retry the run",
    ]);
  });
});
