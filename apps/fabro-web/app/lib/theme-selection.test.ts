import { describe, expect, test } from "bun:test";
import { buildThemeBootScript, resolveTheme } from "./theme-selection";

describe("resolveTheme", () => {
  test("defaults to dark when no saved theme exists", () => {
    expect(resolveTheme(null)).toBe("dark");
    expect(resolveTheme("system")).toBe("dark");
  });

  test("uses the saved theme when it is valid", () => {
    expect(resolveTheme("light")).toBe("light");
    expect(resolveTheme("dark")).toBe("dark");
  });
});

describe("buildThemeBootScript", () => {
  test("defaults the boot script to dark without using system preference", () => {
    const script = buildThemeBootScript();

    expect(script).toContain('localStorage.getItem("fabro-theme")');
    expect(script).toContain('t="dark"');
    expect(script).not.toContain("matchMedia");
  });
});
