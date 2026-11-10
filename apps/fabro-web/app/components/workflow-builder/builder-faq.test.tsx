import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import type { ReactNode } from "react";
import { RouterProvider, createMemoryRouter } from "react-router";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv, textContent } from "../../lib/test-utils";
import { NODE_KINDS } from "../../lib/workflow-builder/catalog";
import { newDraft } from "../../lib/workflow-builder/model";

// Headless UI's Dialog needs a DOM. Its focus trap, Escape and outside click are its own and
// are checked in the browser. These tests cover what the FAQ hands it and what it says.
let dialogProps: { open?: boolean; onClose?: () => void } = {};
const actualHeadless = await import("@headlessui/react");
mock.module("@headlessui/react", () => ({
  ...actualHeadless,
  Dialog: ({ children, open, onClose }: { children: ReactNode; open: boolean; onClose: () => void }) => {
    // The builder also mounts a closed unsaved-changes dialog. Only the open one counts here.
    if (open) dialogProps = { open, onClose };
    return open ? <div role="dialog">{children}</div> : null;
  },
  DialogPanel: ({ children, className }: { children: ReactNode; className?: string }) => (
    <div className={className}>{children}</div>
  ),
  DialogTitle: ({ children, className }: { children: ReactNode; className?: string }) => (
    <h2 className={className}>{children}</h2>
  ),
}));
// The canvas draws with React Flow, which needs a DOM. The toolbar's text is checked below.
mock.module("./canvas", () => ({ WorkflowCanvas: () => null }));

const { BUILDER_FAQ, BuilderFaq } = await import("./builder-faq");
const WorkflowBuilder = (await import("./builder")).default;

function source(file: string): string {
  return readFileSync(new URL(file, import.meta.url), "utf8");
}

function answerText(question: string): string {
  const entry = BUILDER_FAQ.find((candidate) => candidate.question === question);
  if (!entry) throw new Error(`no question: ${question}`);
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(<div>{entry.answer}</div>);
  });
  if (!renderer) throw new Error("not rendered");
  return textContent(renderer.root);
}

let teardown: () => void;
beforeEach(() => {
  teardown = setupReactTestEnv();
  dialogProps = {};
});
afterEach(() => teardown());

describe("what the FAQ says", () => {
  test("answers the two questions people asked, and opens with them", () => {
    expect(BUILDER_FAQ.slice(0, 2).map((entry) => entry.question)).toEqual([
      "How do I add a stage?",
      "How do I delete a node?",
    ]);
    expect(BUILDER_FAQ.filter((entry) => entry.defaultOpen)).toHaveLength(2);
  });

  test("covers what a beginner runs into next", () => {
    const questions = BUILDER_FAQ.map((entry) => entry.question).join(" ");
    for (const topic of ["stages control", "prompts and files", "saved", "Visual, Code and Issues", "run a workflow"]) {
      expect(questions).toContain(topic);
    }
  });

  test("adding a stage says dragging doesn't assign a node, and names both ways to assign one", () => {
    const answer = answerText("How do I add a stage?");
    expect(answer).toContain("Add stage");
    expect(answer).toContain("Nodes");
    expect(answer).toContain("Stage menu");
    expect(answer).toContain("Dragging a node over a stage doesn't add it");
  });

  test("deleting names the keys, the button, undo, and why a key might only edit text", () => {
    const answer = answerText("How do I delete a node?");
    for (const part of ["Delete", "Backspace", "Delete node", "Undo", "text field"]) {
      expect(answer).toContain(part);
    }
  });

  test("the prompt answer names exactly the kinds that take prompts", () => {
    const kinds = NODE_KINDS.filter((kind) => kind.supportsPrompt).map((kind) => kind.label);
    expect(kinds).toEqual(["Agent", "Prompt", "Fan-in"]);
    expect(answerText("How do I add prompts and files?")).toContain("Agent, Prompt or Fan-in node");
    expect(answerText("How do I add prompts and files?")).toContain("up to 1 MB");
  });

  test("saving says it stays in this browser and how to keep a copy", () => {
    const answer = answerText("Where is my work saved?");
    expect(answer).toContain("In this browser");
    expect(answer).toContain("Download ZIP bundle");
  });

  test("running says plainly that the builder doesn't, and gives the sample's real commands", () => {
    const answer = answerText("How do I run a workflow?");
    expect(answer).toContain("The builder doesn't start runs");
    const readme = source("../../lib/workflow-builder/example.ts");
    for (const command of [
      "fabro validate .fabro/workflows/frontend-component-changes/workflow.fabro",
      "fabro preflight .fabro/workflows/frontend-component-changes/workflow.toml",
      "fabro run frontend-component-changes",
    ]) {
      expect(answer).toContain(command);
      expect(readme).toContain(command);
    }
    expect(readme).toContain("fabro repo init");
  });

  // Words on a screen can drift from the code. Each label the FAQ tells someone to find has to
  // still be there, in the file that draws it.
  test.each([
    ["Add stage", "./canvas.tsx"],
    ["Add stage", "./inspector-graph.tsx"],
    ["Stages", "./inspector-graph.tsx"],
    ["Files in this draft", "./inspector-graph.tsx"],
    ["Nodes", "./inspector-group.tsx"],
    ["Delete stage", "./inspector-group.tsx"],
    ["Stage", "./inspector-node.tsx"],
    ["Delete node", "./inspector-node.tsx"],
    ["Add prompt", "./prompt-editor.tsx"],
    ["Choose file", "./prompt-editor.tsx"],
    ["Restore last valid code", "./code-editor.tsx"],
    ["Download ZIP bundle", "./builder.tsx"],
    ["Import as new draft…", "./builder.tsx"],
    ["More actions", "./builder.tsx"],
  ])("the label %s exists in %s", (label, file) => {
    expect(source(file)).toContain(label);
  });

  test("the behavior it describes is the behavior in the code", () => {
    const canvas = source("./canvas.tsx");
    // Delete and Backspace are canvas shortcuts that step aside in a text field.
    expect(canvas).toContain('event.key === "Delete" || event.key === "Backspace"');
    expect(canvas).toContain('target.closest("input, textarea, select');
    // React Flow's own delete key is off, so this handler is the only one.
    expect(canvas).toContain("deleteKeyCode={null}");
    // A stage's frame moves by its title.
    expect(source("./canvas-model.ts")).toContain('dragHandle: ".wfb-group-header"');
  });
});

describe("the dialog", () => {
  function renderFaq(open: boolean, onClose = () => undefined) {
    let renderer: TestRenderer.ReactTestRenderer | undefined;
    act(() => {
      renderer = TestRenderer.create(<BuilderFaq open={open} onClose={onClose} />);
    });
    if (!renderer) throw new Error("not rendered");
    return renderer;
  }

  test("renders nothing until it is opened", () => {
    expect(renderFaq(false).toJSON()).toBeNull();
  });

  test("has a title and every question, with the first two open", () => {
    const { root } = renderFaq(true);
    expect(textContent(root.findByType("h2"))).toBe("Builder help");
    const items = root.findAllByType("details");
    expect(items).toHaveLength(BUILDER_FAQ.length);
    expect(items.map((item) => Boolean(item.props.open))).toEqual(BUILDER_FAQ.map((entry) => Boolean(entry.defaultOpen)));
  });

  test("focus starts on the first question, not the close icon", () => {
    const summaries = renderFaq(true).root.findAllByType("summary");
    expect(summaries[0].props["data-autofocus"]).toBe(true);
    expect(summaries.slice(1).every((summary) => summary.props["data-autofocus"] === undefined)).toBe(true);
  });

  test("the close button has a name and closes it", () => {
    const onClose = mock(() => undefined);
    const button = renderFaq(true, onClose).root.findByProps({ "aria-label": "Close help" });
    act(() => button.props.onClick());
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("takes the builder's theme with it into the portal", () => {
    const panel = renderFaq(true).root.findAll((node) => node.type === "div" && String(node.props.className).includes("modal"));
    expect(panel.some((node) => String(node.props.className).includes("wfb-theme"))).toBe(true);
  });

  test("a command block that scrolls sideways can be reached by keyboard", () => {
    const pre = renderFaq(true).root.findByType("pre");
    expect(pre.props.tabIndex).toBe(0);
    expect(pre.props["aria-label"]).toBeTruthy();
  });
});

describe("the FAQ button", () => {
  async function renderBuilder() {
    const draft = newDraft("FAQ test");
    const router = createMemoryRouter(
      [{ path: "*", element: <WorkflowBuilder initialDraft={draft} persisted={false} onFirstSave={() => undefined} /> }],
      { initialEntries: ["/workflows/new"] },
    );
    let renderer: TestRenderer.ReactTestRenderer | undefined;
    await act(async () => {
      renderer = TestRenderer.create(<RouterProvider router={router} />);
    });
    if (!renderer) throw new Error("not rendered");
    return renderer;
  }

  function headerButtons(renderer: TestRenderer.ReactTestRenderer) {
    const actions = renderer.root.findByProps({ className: "wfb-header-actions" });
    return actions.findAll((node) => node.type === "button").map((node) => ({ node, text: node.props["aria-label"] ?? textContent(node).trim() }));
  }

  test("sits immediately before Settings", async () => {
    const texts = headerButtons(await renderBuilder()).map((button) => button.text);
    expect(texts.indexOf("Builder help")).toBeGreaterThanOrEqual(0);
    expect(texts.indexOf("Settings")).toBe(texts.indexOf("Builder help") + 1);
  });

  test("opens the FAQ and says it is a dialog", async () => {
    const renderer = await renderBuilder();
    const find = () => headerButtons(renderer).find((button) => button.text === "Builder help")?.node;
    expect(find()?.props["aria-haspopup"]).toBe("dialog");
    expect(find()?.props["aria-expanded"]).toBe(false);
    expect(renderer.root.findAll((node) => node.type === "details")).toHaveLength(0);

    act(() => find()?.props.onClick());
    expect(find()?.props["aria-expanded"]).toBe(true);
    expect(renderer.root.findAll((node) => node.type === "details")).toHaveLength(BUILDER_FAQ.length);

    act(() => dialogProps.onClose?.());
    expect(find()?.props["aria-expanded"]).toBe(false);
    expect(renderer.root.findAll((node) => node.type === "details")).toHaveLength(0);
  });

  test("opening it changes nothing in the draft", async () => {
    const renderer = await renderBuilder();
    const status = () => renderer.root.findByProps({ className: "wfb-save-status" }).children.join("");
    const before = status();
    act(() => headerButtons(renderer).find((button) => button.text === "Builder help")?.node.props.onClick());
    expect(status()).toBe(before);
    expect(before).toBe("Not saved yet");
  });
});

describe("the canvas toolbar", () => {
  test("Add stage shows its name, in the same style as Add node", () => {
    const canvas = source("./canvas.tsx");
    expect(canvas).toContain("<span>Add stage</span>");
    expect(canvas).toContain("<span>Add node</span>");
    const stageButton = canvas.slice(canvas.lastIndexOf("<button", canvas.indexOf("<span>Add stage</span>")));
    expect(stageButton.startsWith('<button type="button" className="wfb-tool wfb-tool-add" disabled={readOnly}')).toBe(true);
  });
});
