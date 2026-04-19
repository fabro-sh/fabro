import { describe, expect, test } from "bun:test";
import { getVisibleNavigation } from "./app-shell";

describe("getVisibleNavigation", () => {
  test("shows all nav items in demo mode", () => {
    const items = getVisibleNavigation(true);
    const names = items.map((i) => i.name);
    expect(names).toContain("Workflows");
    expect(names).toContain("Runs");
    expect(names).toContain("Insights");
    expect(names).toContain("Settings");
  });

  test("hides Workflows and Insights in production mode", () => {
    const items = getVisibleNavigation(false);
    const names = items.map((i) => i.name);
    expect(names).not.toContain("Workflows");
    expect(names).not.toContain("Insights");
    expect(names).toContain("Runs");
    expect(names).toContain("Settings");
  });
});
