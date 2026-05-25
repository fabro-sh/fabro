import { describe, expect, test } from "bun:test";
import { getVisibleNavigation } from "./navigation";

describe("getVisibleNavigation", () => {
  test("shows Automations first in demo mode followed by Runs and Settings", () => {
    const items = getVisibleNavigation(true);
    const names = items.map((i) => i.name);
    expect(names).toEqual(["Automations", "Runs", "Settings"]);
  });

  test("hides Automations in production mode", () => {
    const items = getVisibleNavigation(false);
    const names = items.map((i) => i.name);
    expect(names).not.toContain("Automations");
    expect(names).toContain("Runs");
    expect(names).toContain("Settings");
  });
});
