import { describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { SWRConfig } from "swr";
import {
  type ApiQuestion,
  QuestionType,
} from "@qltysh/fabro-api-client";

import {
  InterviewDock,
  loadDockHeight,
  clampDockHeight,
  saveDockHeight,
} from "./interview-dock";
import { displayLabel } from "./interview-label";
import { generatedAxios } from "../lib/api-client";

function render(node: React.ReactNode): TestRenderer.ReactTestRenderer {
  let tree: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    tree = TestRenderer.create(
      <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
        {node}
      </SWRConfig>,
    );
  });
  return tree!;
}

function textContent(node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textContent).join("");
  return (node.children ?? []).map(textContent).join("");
}

function instanceText(instance: TestRenderer.ReactTestInstance): string {
  const parts: string[] = [];
  for (const child of instance.children) {
    if (typeof child === "string") parts.push(child);
    else parts.push(instanceText(child));
  }
  return parts.join("");
}

function buttonsByText(
  tree: TestRenderer.ReactTestRenderer,
): Record<string, TestRenderer.ReactTestInstance> {
  const result: Record<string, TestRenderer.ReactTestInstance> = {};
  for (const button of tree.root.findAllByType("button")) {
    const label = instanceText(button).trim();
    if (label) result[label] = button;
  }
  return result;
}

function makeQuestion(overrides: Partial<ApiQuestion> = {}): ApiQuestion {
  return {
    id: "q-1",
    text: "Approve the deployment plan?",
    stage: "approve_plan",
    question_type: QuestionType.YES_NO,
    options: [],
    allow_freeform: false,
    timeout_seconds: null,
    context_display: null,
    ...overrides,
  };
}

describe("InterviewDock", () => {
  test("renders successfully when localStorage is empty", () => {
    const storage = new Map<string, string>();
    globalThis.window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      },
      innerHeight: 1000,
    } as any;

    const tree = render(
      <InterviewDock runId="run-1" questions={[makeQuestion()]} />,
    );
    expect(tree.toJSON()).not.toBeNull();
    // Should not persist height on initial mount
    expect(storage.size).toBe(0);
  });

  test("renders question text and stage in the header", () => {
    const tree = render(
      <InterviewDock runId="run-1" questions={[makeQuestion()]} />,
    );
    const text = textContent(tree.toJSON());
    expect(text).toContain("Approve the deployment plan?");
    expect(text).toContain("approve_plan");
    expect(text).toContain("Awaiting input");
  });

  test("yes/no question shows two buttons", () => {
    const tree = render(
      <InterviewDock runId="run-1" questions={[makeQuestion()]} />,
    );
    const buttons = buttonsByText(tree);
    expect(buttons.Yes).toBeDefined();
    expect(buttons.No).toBeDefined();
  });

  test("yes/no question submits typed yes and no answers", async () => {
    const submitted: unknown[] = [];
    const originalAdapter = generatedAxios.defaults.adapter;
    generatedAxios.defaults.adapter = async (config) => {
      submitted.push(JSON.parse(String(config.data)));
      return {
        data: undefined,
        status: 204,
        statusText: "No Content",
        headers: {},
        config,
      };
    };

    try {
      const tree = render(
        <InterviewDock runId="run-1" questions={[makeQuestion()]} />,
      );
      const buttons = buttonsByText(tree);

      await act(async () => {
        buttons.Yes.props.onClick();
        await Promise.resolve();
      });
      await act(async () => {
        buttons.No.props.onClick();
        await Promise.resolve();
      });

      expect(submitted).toEqual([{ kind: "yes" }, { kind: "no" }]);
    } finally {
      generatedAxios.defaults.adapter = originalAdapter;
    }
  });

  test("multiple choice question renders option buttons with stripped accelerator prefixes", () => {
    const question = makeQuestion({
      question_type: QuestionType.MULTIPLE_CHOICE,
      options: [
        { key: "A", label: "[A] Approve" },
        { key: "R", label: "[R] Revise" },
      ],
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    const buttons = buttonsByText(tree);
    expect(buttons.Approve).toBeDefined();
    expect(buttons.Revise).toBeDefined();
  });

  test("multiple choice renders option descriptions as display text", () => {
    const question = makeQuestion({
      question_type: QuestionType.MULTIPLE_CHOICE,
      options: [
        {
          key: "A",
          label: "[A] Approve",
          description: "Deploy the current patch",
          preview: "<b>not rendered specially</b>",
        },
      ],
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    const text = textContent(tree.toJSON());
    expect(text).toContain("Approve");
    expect(text).toContain("Deploy the current patch");
    expect(text).not.toContain("<b>not rendered specially</b>");
  });

  test("freeform question renders a textarea and disables send when empty", () => {
    const question = makeQuestion({
      question_type: QuestionType.FREEFORM,
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    const textareas = tree.root.findAllByType("textarea");
    expect(textareas).toHaveLength(1);
    const sendButton = tree.root.findByProps({ type: "submit" });
    expect(sendButton.props.disabled).toBe(true);
  });

  test("multi-select shows submit button disabled until at least one option is selected", () => {
    const question = makeQuestion({
      question_type: QuestionType.MULTI_SELECT,
      options: [
        { key: "a", label: "[A] Apples" },
        { key: "b", label: "[B] Bananas" },
      ],
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    const buttons = buttonsByText(tree);
    const submit = buttons["Submit selection"];
    expect(submit).toBeDefined();
    expect(submit.props.disabled).toBe(true);

    act(() => {
      buttons.Apples.props.onClick();
    });
    const submitAfter = buttonsByText(tree)["Submit selection"];
    expect(submitAfter.props.disabled).toBe(false);
  });

  test("multiple choice with allow_freeform renders both buttons and a textarea", () => {
    const question = makeQuestion({
      question_type: QuestionType.MULTIPLE_CHOICE,
      allow_freeform: true,
      options: [{ key: "A", label: "[A] Approve" }],
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    expect(buttonsByText(tree).Approve).toBeDefined();
    expect(tree.root.findAllByType("textarea")).toHaveLength(1);
  });

  test("shows '+N more pending' pill when multiple questions are queued", () => {
    const tree = render(
      <InterviewDock
        runId="run-1"
        questions={[
          makeQuestion({ id: "q-1", stage: "stage-a" }),
          makeQuestion({ id: "q-2", stage: "stage-b" }),
          makeQuestion({ id: "q-3", stage: "stage-c" }),
        ]}
      />,
    );
    const text = textContent(tree.toJSON());
    expect(text).toContain("2");
    expect(text).toContain("more pending");
  });

  test("renders nothing when questions list is empty", () => {
    const tree = render(<InterviewDock runId="run-1" questions={[]} />);
    expect(tree.toJSON()).toBeNull();
  });

  test("notifies parent of height on mount", () => {
    const storage = new Map<string, string>();
    globalThis.window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      },
      innerHeight: 1000,
    } as any;

    const heights: string[] = [];
    const onDockHeightChange = (height: string) => heights.push(height);

    render(
      <InterviewDock
        runId="run-1"
        questions={[makeQuestion()]}
        onDockHeightChange={onDockHeightChange}
      />,
    );

    expect(heights.length).toBe(1);
    expect(heights[0]).toMatch(/^\d+(?:\.\d+)?(rem|px|vh)$/);
  });

  test("renders the optional context_display section", () => {
    const question = makeQuestion({
      context_display: "Plan:\n1. Deploy\n2. Verify",
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    const text = textContent(tree.toJSON());
    expect(text).toContain("Context from preceding stage");
    expect(text).toContain("1. Deploy");
  });

  test("renders resize handle with correct ARIA attributes", () => {
    const tree = render(
      <InterviewDock runId="run-1" questions={[makeQuestion()]} />,
    );
    const handles = tree.root.findAllByProps({ role: "separator" });
    expect(handles).toHaveLength(1);
    const handle = handles[0];
    expect(handle.props["aria-orientation"]).toBe("horizontal");
    expect(handle.props["aria-label"]).toBe("Resize interview dock");
  });

  test("notifies parent when resize starts", () => {
    const resizeActiveCalls: boolean[] = [];
    const tree = render(
      <InterviewDock
        runId="run-1"
        questions={[makeQuestion()]}
        onResizeActiveChange={(active) => resizeActiveCalls.push(active)}
      />,
    );
    const handle = tree.root.findByProps({ role: "separator" });

    act(() => {
      handle.props.onPointerDown({
        preventDefault: () => {},
        pointerId: 123,
        clientY: 500,
        currentTarget: { setPointerCapture: () => {} },
      });
    });

    expect(resizeActiveCalls).toEqual([true]);
  });

  test("resizing upward increases dock height", () => {
    globalThis.window = {
      localStorage: {
        getItem: () => null,
        setItem: () => {},
      },
      innerHeight: 1000,
    } as any;

    const heightChanges: string[] = [];
    const tree = render(
      <InterviewDock
        runId="run-1"
        questions={[makeQuestion()]}
        onDockHeightChange={(height) => heightChanges.push(height)}
      />,
    );
    const handle = tree.root.findByProps({ role: "separator" });

    const initialHeight = parseFloat(heightChanges[0]);

    act(() => {
      handle.props.onPointerDown({
        preventDefault: () => {},
        pointerId: 1,
        clientY: 500,
        currentTarget: { setPointerCapture: () => {} },
      });
    });

    act(() => {
      handle.props.onPointerMove({ clientY: 400 });
    });

    const newHeight = parseFloat(heightChanges[heightChanges.length - 1]);
    expect(newHeight).toBeGreaterThan(initialHeight);
  });

  test("resizing respects minimum height", () => {
    globalThis.window = {
      localStorage: {
        getItem: () => null,
        setItem: () => {},
      },
      innerHeight: 1000,
    } as any;

    const heightChanges: string[] = [];
    const tree = render(
      <InterviewDock
        runId="run-1"
        questions={[makeQuestion()]}
        onDockHeightChange={(height) => heightChanges.push(height)}
      />,
    );
    const handle = tree.root.findByProps({ role: "separator" });

    act(() => {
      handle.props.onPointerDown({
        preventDefault: () => {},
        pointerId: 1,
        clientY: 500,
        currentTarget: { setPointerCapture: () => {} },
      });
    });

    // Try to drag far downward to shrink below minimum
    act(() => {
      handle.props.onPointerMove({ clientY: 5000 });
    });

    // Should be clamped to minimum (12rem = 192px)
    expect(heightChanges[heightChanges.length - 1]).toBe("192px");
  });

  test("notifies parent when resize ends", () => {
    globalThis.window = {
      localStorage: {
        getItem: () => null,
        setItem: () => {},
      },
      innerHeight: 1000,
    } as any;

    const resizeActiveCalls: boolean[] = [];
    const tree = render(
      <InterviewDock
        runId="run-1"
        questions={[makeQuestion()]}
        onResizeActiveChange={(active) => resizeActiveCalls.push(active)}
      />,
    );
    const handle = tree.root.findByProps({ role: "separator" });

    act(() => {
      handle.props.onPointerDown({
        preventDefault: () => {},
        pointerId: 123,
        clientY: 500,
        currentTarget: { setPointerCapture: () => {} },
      });
    });

    act(() => {
      handle.props.onPointerUp({
        pointerId: 123,
        currentTarget: {
          releasePointerCapture: () => {},
        },
      });
    });

    expect(resizeActiveCalls).toEqual([true, false]);
  });

  test("notifies parent when resize is cancelled", () => {
    globalThis.window = {
      localStorage: {
        getItem: () => null,
        setItem: () => {},
      },
      innerHeight: 1000,
    } as any;

    const resizeActiveCalls: boolean[] = [];
    const tree = render(
      <InterviewDock
        runId="run-1"
        questions={[makeQuestion()]}
        onResizeActiveChange={(active) => resizeActiveCalls.push(active)}
      />,
    );
    const handle = tree.root.findByProps({ role: "separator" });

    act(() => {
      handle.props.onPointerDown({
        preventDefault: () => {},
        pointerId: 456,
        clientY: 500,
        currentTarget: { setPointerCapture: () => {} },
      });
    });

    act(() => {
      handle.props.onPointerCancel({
        pointerId: 456,
        currentTarget: {
          releasePointerCapture: () => {},
        },
      });
    });

    expect(resizeActiveCalls).toEqual([true, false]);
  });
});

describe("loadDockHeight", () => {
  test("returns a valid height when localStorage is empty", () => {
    const storage = new Map<string, string>();
    globalThis.window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      },
      innerHeight: 1000,
    } as any;

    const height = loadDockHeight();
    expect(height).toMatch(/^\d+(?:\.\d+)?(rem|px|vh)$/);
  });

  test("restores previously saved height", () => {
    const storage = new Map<string, string>([["fabro.interviewDock.height", "24rem"]]);
    globalThis.window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      },
      innerHeight: 1000,
    } as any;

    expect(loadDockHeight()).toBe("24rem");
  });

  test("ignores invalid stored values", () => {
    const storage = new Map<string, string>([["fabro.interviewDock.height", "invalid"]]);
    globalThis.window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      },
      innerHeight: 1000,
    } as any;

    const height = loadDockHeight();
    expect(height).toMatch(/^\d+(?:\.\d+)?(rem|px|vh)$/);
  });

  test("clamps stored height that is too small", () => {
    const storage = new Map<string, string>([["fabro.interviewDock.height", "8rem"]]);
    globalThis.window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      },
      innerHeight: 1000,
    } as any;

    expect(loadDockHeight()).toBe("12rem");
  });

  test("clamps stored height that is too large", () => {
    const storage = new Map<string, string>([["fabro.interviewDock.height", "90vh"]]);
    globalThis.window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      },
      innerHeight: 1000,
    } as any;

    const result = loadDockHeight();
    expect(result).toBe("80vh");
  });
});

describe("saveDockHeight", () => {
  test("persists height to localStorage", () => {
    const storage = new Map<string, string>();
    globalThis.window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      },
      innerHeight: 1000,
    } as any;

    saveDockHeight("24rem");
    expect(storage.get("fabro.interviewDock.height")).toBe("24rem");
  });

  test("handles localStorage errors gracefully", () => {
    globalThis.window = {
      localStorage: {
        getItem: () => { throw new Error("Quota exceeded"); },
        setItem: () => { throw new Error("Quota exceeded"); },
      },
      innerHeight: 1000,
    } as any;

    expect(() => saveDockHeight("24rem")).not.toThrow();
  });
});

describe("clampDockHeight", () => {
  test("accepts valid heights within bounds", () => {
    expect(clampDockHeight("18rem")).toBe("18rem");
    expect(clampDockHeight("300px")).toBe("300px");
    expect(clampDockHeight("50vh")).toBe("50vh");
  });

  test("enforces minimum height", () => {
    expect(clampDockHeight("8rem")).toBe("12rem");
    expect(clampDockHeight("100px")).toBe("12rem");
  });

  test("enforces maximum height", () => {
    expect(clampDockHeight("90vh")).toBe("80vh");
  });

  test("rejects invalid height formats", () => {
    const result = clampDockHeight("invalid");
    expect(result).toMatch(/^\d+(?:\.\d+)?(rem|px|vh)$/);

    expect(clampDockHeight("")).toMatch(/^\d+(?:\.\d+)?(rem|px|vh)$/);
    expect(clampDockHeight("100")).toMatch(/^\d+(?:\.\d+)?(rem|px|vh)$/);
  });
});

describe("displayLabel", () => {
  test("strips bracketed accelerator", () => {
    expect(displayLabel("[A] Approve")).toBe("Approve");
  });

  test("strips parenthesis accelerator", () => {
    expect(displayLabel("Y) Yes, deploy")).toBe("Yes, deploy");
  });

  test("strips dash accelerator", () => {
    expect(displayLabel("Y - Yes, deploy")).toBe("Yes, deploy");
  });

  test("returns original label when no accelerator pattern matches", () => {
    expect(displayLabel("Plain label")).toBe("Plain label");
  });

  test("falls back to original label when stripping yields empty string", () => {
    expect(displayLabel("[A]")).toBe("[A]");
  });
});
