import { describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { SWRConfig } from "swr";
import {
  type ApiQuestion,
  QuestionType,
  ReviewTargetKind,
} from "@qltysh/fabro-api-client";

import {
  contextPreview,
  InterviewDock,
  shouldStackOptions,
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
  test("renders question text and stage in the header", () => {
    const tree = render(
      <InterviewDock runId="run-1" questions={[makeQuestion()]} />,
    );
    const text = textContent(tree.toJSON());
    expect(text).toContain("Approve the deployment plan?");
    expect(text).toContain("approve_plan");
    expect(text).toContain("Awaiting input");
  });

  test("renders the review target as the link in the question", () => {
    const url =
      "https://quarry.lithos.computer/tmp/0123456789abcdef0123456789abcdef";
    const tree = render(
      <InterviewDock
        runId="run-1"
        questions={[
          makeQuestion({
            text: "Review the Quarry review exercise document, then choose the next action.",
            review_target: {
              label: "Quarry review exercise",
              url,
              kind: ReviewTargetKind.DOCUMENT,
            },
          }),
        ]}
      />,
    );

    expect(textContent(tree.toJSON())).toContain(
      "Review the Quarry review exercise document, then choose the next action.",
    );
    const links = tree.root.findAllByType("a");
    expect(links).toHaveLength(1);
    expect(links[0].props.href).toBe(url);
    expect(links[0].props.target).toBe("_blank");
    expect(links[0].props.rel).toBe("noopener noreferrer");
    expect(links[0].props.referrerPolicy).toBe("no-referrer");
  });

  test("does not link an unsafe review target received from the API", () => {
    const fallback = "Review the document, then choose the next action.";
    const tree = render(
      <InterviewDock
        runId="run-1"
        questions={[
          makeQuestion({
            text: fallback,
            review_target: {
              label: "Unsafe target",
              url: "javascript:alert(1)",
              kind: ReviewTargetKind.DOCUMENT,
            },
          }),
        ]}
      />,
    );

    expect(textContent(tree.toJSON())).toContain(fallback);
    expect(tree.root.findAllByType("a")).toHaveLength(0);
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

  test("multiple choice renders option descriptions and previews as display text", () => {
    const question = makeQuestion({
      question_type: QuestionType.MULTIPLE_CHOICE,
      options: [
        {
          key: "A",
          label: "[A] Approve",
          description: "Deploy the current patch",
          preview: "<b>not rendered specially</b>",
        },
        { key: "R", label: "[R] Revise" },
      ],
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    const text = textContent(tree.toJSON());
    expect(text).toContain("Approve");
    expect(text).toContain("Deploy the current patch");
    // The preview is shown as the text it is, never parsed as markup.
    expect(text).toContain("<b>not rendered specially</b>");
    const previews = tree.root.findAll(
      (node) => node.props["data-testid"] === "interview-option-preview",
    );
    expect(previews).toHaveLength(1);
    expect(shouldStackOptions(question.options ?? [])).toBe(true);
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

  test("context starts closed so it costs one line, not a standing panel", () => {
    const question = makeQuestion({
      context_display: "Plan:\n1. Deploy\n2. Verify",
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    const details = tree.root.findAllByType("details");
    expect(details).toHaveLength(1);
    expect(details[0]!.props.open).toBeFalsy();
  });

  test("omits the question type subtitle that the answer buttons already state", () => {
    const question = makeQuestion({
      question_type: QuestionType.MULTIPLE_CHOICE,
      options: [{ key: "A", label: "[A] Approve" }],
    });
    const tree = render(
      <InterviewDock runId="run-1" questions={[question]} />,
    );
    expect(textContent(tree.toJSON())).not.toContain("Pick one");
  });

  test("collapsing hides the answer controls but keeps the header", () => {
    const tree = render(
      <InterviewDock runId="run-1" questions={[makeQuestion()]} />,
    );
    const toggle = tree.root.findByProps({ "aria-label": "Collapse Interview question" });

    act(() => {
      toggle.props.onClick();
    });

    const expanded = tree.root.findByProps({
      "aria-label": "Expand Interview question",
    });
    expect(expanded.props["aria-expanded"]).toBe(false);
    expect(textContent(tree.toJSON())).toContain("Awaiting input");
  });

  test("a queued question arrives expanded even after the panel was collapsed", () => {
    const questions = [
      makeQuestion({ id: "q-1", stage: "stage-a" }),
      makeQuestion({ id: "q-2", stage: "stage-b" }),
    ];
    const tree = render(<InterviewDock runId="run-1" questions={questions} />);

    act(() => {
      tree.root
        .findByProps({ "aria-label": "Collapse Interview question" })
        .props.onClick();
    });
    act(() => {
      buttonsByText(tree)["1 more pending"]!.props.onClick();
    });

    const toggle = tree.root.findByProps({
      "aria-label": "Collapse Interview question",
    });
    expect(toggle.props["aria-expanded"]).toBe(true);
  });
});

describe("shouldStackOptions", () => {
  test("keeps short labels as a wrapping row", () => {
    expect(
      shouldStackOptions([
        { key: "a", label: "Approve" },
        { key: "b", label: "Revise" },
      ]),
    ).toBe(false);
  });

  test("stacks once a label is too long to sit in a pill", () => {
    expect(
      shouldStackOptions([
        { key: "a", label: "Approve" },
        { key: "b", label: "Review complete; sync the current Markdown document" },
      ]),
    ).toBe(true);
  });

  test("stacks when any option carries a description", () => {
    expect(
      shouldStackOptions([
        { key: "a", label: "Approve", description: "Deploy the current patch" },
      ]),
    ).toBe(true);
  });
});

describe("contextPreview", () => {
  test("uses the first non-empty line", () => {
    expect(contextPreview("\n\nPlan is ready\nSecond line")).toBe("Plan is ready");
  });

  test("truncates a long first line", () => {
    const preview = contextPreview("x".repeat(200));
    expect(preview).toHaveLength(61);
    expect(preview.endsWith("…")).toBe(true);
  });

  test("returns an empty string for blank context", () => {
    expect(contextPreview("   \n  ")).toBe("");
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
