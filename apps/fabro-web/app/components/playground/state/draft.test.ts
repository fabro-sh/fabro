import { describe, expect, test } from "bun:test";

import {
  ALL_SHAPES,
  EXIT_ID,
  RESERVED_IDS,
  START_ID,
  createInitialDraft,
  isValidNodeId,
  isValidShape,
  isValidWorkflowName,
  isWelcomeState,
} from "./draft";

describe("createInitialDraft", () => {
  test("welcome state has only start and exit", () => {
    const draft = createInitialDraft();
    expect(draft.nodes).toHaveLength(2);
    expect(draft.nodes.map((n) => n.id).sort()).toEqual([EXIT_ID, START_ID].sort());
    expect(draft.edges).toEqual([{ from: START_ID, to: EXIT_ID }]);
  });

  test("uses reserved shapes for terminals", () => {
    const draft = createInitialDraft();
    const start = draft.nodes.find((n) => n.id === START_ID);
    const exit = draft.nodes.find((n) => n.id === EXIT_ID);
    expect(start?.shape).toBe("mdiamond");
    expect(exit?.shape).toBe("msquare");
  });

  test("default name is 'untitled' and goal is empty", () => {
    const draft = createInitialDraft();
    expect(draft.name).toBe("untitled");
    expect(draft.goal).toBe("");
  });
});

describe("isWelcomeState", () => {
  test("true on a fresh draft", () => {
    expect(isWelcomeState(createInitialDraft())).toBe(true);
  });

  test("false once any user node is added", () => {
    const draft = createInitialDraft();
    draft.nodes.push({ id: "plan", label: "Plan", shape: "box" });
    expect(isWelcomeState(draft)).toBe(false);
  });
});

describe("isValidNodeId", () => {
  test.each([
    ["plan", true],
    ["run_tests", true],
    ["step_42", true],
    ["a", true],
    ["Plan", false], // uppercase
    ["1step", false], // leading digit
    ["_hidden", false], // leading underscore
    ["run-tests", false], // hyphen
    ["", false],
  ])("`%s` -> %s", (input, expected) => {
    expect(isValidNodeId(input)).toBe(expected);
  });
});

describe("isValidWorkflowName", () => {
  test("snake_case ok", () => {
    expect(isValidWorkflowName("release_notes")).toBe(true);
  });

  test("rejects uppercase", () => {
    expect(isValidWorkflowName("ReleaseNotes")).toBe(false);
  });
});

describe("isValidShape", () => {
  test("accepts every shape in ALL_SHAPES", () => {
    for (const shape of ALL_SHAPES) {
      expect(isValidShape(shape)).toBe(true);
    }
  });

  test("rejects unknown shapes and non-strings", () => {
    expect(isValidShape("circle")).toBe(false);
    expect(isValidShape(42)).toBe(false);
    expect(isValidShape(null)).toBe(false);
    expect(isValidShape(undefined)).toBe(false);
  });
});

describe("RESERVED_IDS", () => {
  test("covers start and exit", () => {
    expect(RESERVED_IDS).toContain(START_ID);
    expect(RESERVED_IDS).toContain(EXIT_ID);
  });
});
