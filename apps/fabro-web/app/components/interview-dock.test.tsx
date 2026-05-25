import { describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { SWRConfig } from "swr";
import {
  type ApiQuestion,
  QuestionType,
} from "@qltysh/fabro-api-client";

import { InterviewDock } from "./interview-dock";
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
