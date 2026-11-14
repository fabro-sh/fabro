import { describe, expect, test } from "bun:test";

import { dotColorVariable, faviconWithDot } from "./run-favicon";
import { runStatusDisplay, type RunStatus } from "../data/runs";

describe("dotColorVariable", () => {
  test("uses the colour of the run's badge", () => {
    expect(dotColorVariable("failed")).toBe("--color-coral");
    expect(dotColorVariable("dead")).toBe("--color-coral");
    expect(dotColorVariable("blocked")).toBe("--color-amber");
    expect(dotColorVariable("paused")).toBe("--color-amber");
    expect(dotColorVariable("succeeded")).toBe("--color-mint");
  });

  test("draws a running run in cyan, since the badge's teal is the logo's own colour", () => {
    expect(dotColorVariable("running")).toBe("--color-cyan-500");
  });

  test("gives muted and unknown statuses no dot", () => {
    for (const kind of ["submitted", "pending", "removing", "archived"]) {
      expect(dotColorVariable(kind)).toBeNull();
    }
    expect(dotColorVariable("exploded")).toBeNull();
    expect(dotColorVariable(undefined)).toBeNull();
  });

  test("covers every status the badge knows", () => {
    for (const kind of Object.keys(runStatusDisplay) as RunStatus[]) {
      const variable = dotColorVariable(kind);
      expect(variable === null || variable.startsWith("--color-")).toBe(true);
    }
  });
});

describe("faviconWithDot", () => {
  const logo = '<svg viewBox="0 0 272 348"><path d="M0 0"/></svg>';

  test("draws the dot over the logo, inside the same SVG", () => {
    expect(faviconWithDot(logo, { fill: "#E86B6B", ring: "#0F1729" })).toBe(
      '<svg viewBox="0 0 272 348"><path d="M0 0"/><circle cx="196" cy="270" r="76" fill="#E86B6B" stroke="#0F1729" stroke-width="20"/></svg>',
    );
  });

  test("leaves anything that isn't an SVG alone", () => {
    expect(faviconWithDot("not found", { fill: "#E86B6B", ring: "#0F1729" })).toBe("not found");
  });
});
