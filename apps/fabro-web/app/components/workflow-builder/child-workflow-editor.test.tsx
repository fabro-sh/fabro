import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { useReducer, type Dispatch } from "react";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv } from "../../lib/test-utils";
import { newDraft } from "../../lib/workflow-builder/model";
import type { WorkflowDraft } from "../../lib/workflow-builder/types";
import { MAX_ATTACHMENT_BYTES, freeAttachmentPath } from "./attachments";
import { ChildWorkflowEditor } from "./child-workflow-editor";
import { editorReducer, initEditor, type EditorAction, type EditorState } from "./editor-state";

const ORIGINAL = "digraph Child { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
const EDITED = ORIGINAL.replace("digraph Child", "digraph EditedChild");
const CHILD_PATH = "stack.child_workflow";
const CHILD_SOURCE = "stack.child_dot_source";

let cleanup: () => void;
let renderer: TestRenderer.ReactTestRenderer | undefined;
let state: EditorState;
let dispatch: Dispatch<EditorAction>;

beforeEach(() => { cleanup = setupReactTestEnv(); });
afterEach(() => {
  if (renderer) act(() => renderer?.unmount());
  renderer = undefined;
  cleanup();
});

function mount(draft: WorkflowDraft) {
  function Harness() {
    [state, dispatch] = useReducer(editorReducer, draft, initEditor);
    return <ChildWorkflowEditor
      context={state}
      node={state.graph!.nodes.find((node) => node.id === "child")!}
      apply={(change, coalesce) => dispatch({ type: "apply", change, coalesce, now: Date.now() })}
    />;
  }
  act(() => { renderer = TestRenderer.create(<Harness />); });
}

function attachedDraft(): WorkflowDraft {
  return {
    ...newDraft(),
    source: `digraph Parent {
      start [shape=Mdiamond]
      child [shape=house stack.child_workflow="shared/check.fabro"]
      other [shape=house stack.child_workflow="shared/check.fabro"]
      exit [shape=Msquare]
      start -> child -> other -> exit
    }`,
    assets: { "shared/check.fabro": ORIGINAL },
  };
}

function switchMode(label: string) {
  const group = renderer!.root.findByProps({ "aria-label": "Child workflow source" });
  const button = group.findAllByType("button").find((node) => node.children[0] === label)!;
  act(() => button.props.onClick());
}

function editInline(value: string) {
  act(() => renderer!.root.findByType("textarea").props.onChange({ target: { value } }));
}

function childAttrs() { return state.graph!.nodes.find((node) => node.id === "child")!.attrs; }

describe("child workflow source conversion", () => {
  test("unchanged source reuses the original attachment", () => {
    mount(attachedDraft());
    switchMode("Inline source");
    switchMode("Path");
    expect(childAttrs()[CHILD_PATH]).toBe("shared/check.fabro");
    expect(childAttrs()[CHILD_SOURCE]).toBeUndefined();
    expect(state.draft.assets).toEqual({ "shared/check.fabro": ORIGINAL });
  });

  test("edited source is saved beside the original without changing other users of that file", () => {
    mount(attachedDraft());
    switchMode("Inline source");
    editInline(EDITED);
    switchMode("Path");
    expect(childAttrs()[CHILD_PATH]).toBe("shared/check-2.fabro");
    expect(state.draft.assets).toEqual({ "shared/check.fabro": ORIGINAL, "shared/check-2.fabro": EDITED });
    expect(state.graph!.nodes.find((node) => node.id === "other")!.attrs[CHILD_PATH]).toBe("shared/check.fabro");
    expect(childAttrs()[CHILD_SOURCE]).toBeUndefined();
    act(() => dispatch({ type: "undo" }));
    expect(childAttrs()[CHILD_SOURCE]).toBe(EDITED);
    expect(state.draft.assets["shared/check-2.fabro"]).toBeUndefined();
    act(() => dispatch({ type: "redo" }));
    expect(childAttrs()[CHILD_PATH]).toBe("shared/check-2.fabro");
    expect(state.draft.assets["shared/check-2.fabro"]).toBe(EDITED);
  });

  test("an inline child without a remembered path becomes a root-level file", () => {
    const draft = attachedDraft();
    draft.source = draft.source.replace('stack.child_workflow="shared/check.fabro"', `stack.child_dot_source=${JSON.stringify(EDITED)}`);
    mount(draft);
    switchMode("Path");
    expect(childAttrs()[CHILD_PATH]).toBe("child.fabro");
    expect(state.draft.assets["child.fabro"]).toBe(EDITED);
  });

  test("a nested child with supporting files stays attached rather than changing its reference base", () => {
    const draft = attachedDraft();
    draft.assets["shared/check.fabro"] = ORIGINAL.replace("start [", 'graph [goal="@ticket.md"] start [');
    draft.assets["shared/ticket.md"] = "The ticket";
    mount(draft);
    switchMode("Inline source");
    expect(childAttrs()[CHILD_PATH]).toBe("shared/check.fabro");
    expect(childAttrs()[CHILD_SOURCE]).toBeUndefined();
    expect(state.draft.assets).toEqual(draft.assets);
    expect(JSON.stringify(renderer!.toJSON())).toContain("Keep this child as a file");
  });

  describe("a child whose inline source names files, at any depth", () => {
    const naming = ORIGINAL.replace("start [", 'graph [goal="@ticket.md"] start [');
    const holding = (source: string) =>
      `digraph Holder { start [shape=Mdiamond] nested [shape=house ${CHILD_SOURCE}=${JSON.stringify(source)}] exit [shape=Msquare] start -> nested -> exit }`;

    function attachedAt(path: string, source: string, extra: Record<string, string> = {}): WorkflowDraft {
      return {
        ...newDraft(),
        source: `digraph Parent { start [shape=Mdiamond] child [shape=house ${CHILD_PATH}="${path}"] exit [shape=Msquare] start -> child -> exit }`,
        assets: { [path]: source, ...extra },
      };
    }
    function expectStaysAFile(draft: WorkflowDraft, path: string, message: string) {
      mount(draft);
      switchMode("Inline source");
      expect(childAttrs()[CHILD_PATH]).toBe(path);
      expect(childAttrs()[CHILD_SOURCE]).toBeUndefined();
      expect(state.draft.assets).toEqual(draft.assets);
      expect(state.past.length).toBe(0);
      expect(JSON.stringify(renderer!.toJSON())).toContain(message);
    }

    test("a grandchild's file keeps the child in its folder", () => {
      expectStaysAFile(attachedAt("shared/check.fabro", holding(naming), { "shared/ticket.md": "T" }), "shared/check.fabro", "it names other files");
    });

    test("a great-grandchild's file too", () => {
      expectStaysAFile(attachedAt("shared/check.fabro", holding(holding(naming)), { "shared/ticket.md": "T" }), "shared/check.fabro", "it names other files");
    });

    test("a child in the workflow's own folder stays a file too, since Fabro's bundler wouldn't carry its files", () => {
      expectStaysAFile(attachedAt("check.fabro", naming, { "ticket.md": "T" }), "check.fabro", "wouldn't reach the run");
      expect(renderer!.root.findAllByType("textarea")).toEqual([]);
    });

    test("a syntax error in a grandchild keeps a foldered child as a file, since what it names can't be known", () => {
      expectStaysAFile(attachedAt("shared/check.fabro", holding("digraph C { start [")), "shared/check.fabro", "has a syntax error");
    });

    test("a syntax error at the workflow's own folder can move inline, where it can be repaired", () => {
      const broken = "digraph C { start [";
      mount(attachedAt("check.fabro", broken));
      switchMode("Inline source");
      expect(childAttrs()[CHILD_SOURCE]).toBe(broken);
      expect(childAttrs()[CHILD_PATH]).toBeUndefined();
    });

    test("nesting that names nothing moves freely, and back to the same file", () => {
      const plain = holding(holding(ORIGINAL));
      mount(attachedAt("shared/check.fabro", plain));
      switchMode("Inline source");
      expect(childAttrs()[CHILD_SOURCE]).toBe(plain);
      expect(childAttrs()[CHILD_PATH]).toBeUndefined();
      switchMode("Path");
      expect(childAttrs()[CHILD_PATH]).toBe("shared/check.fabro");
      expect(state.draft.assets).toEqual({ "shared/check.fabro": plain });
    });

    test("saving inline source whose grandchild names a file keeps the parent's folder, not the remembered one", () => {
      mount(attachedDraft());
      switchMode("Inline source");
      const source = holding(naming);
      editInline(source);
      switchMode("Path");
      expect(childAttrs()[CHILD_PATH]).toBe("child.fabro");
      expect(state.draft.assets["child.fabro"]).toBe(source);
      expect(state.draft.assets["shared/check.fabro"]).toBe(ORIGINAL);
    });

    test("saving inline source with a syntax error also keeps the parent's folder, and every byte", () => {
      mount(attachedDraft());
      switchMode("Inline source");
      const source = holding('digraph C { graph [goal="@ticket.md"] start [');
      editInline(source);
      switchMode("Path");
      expect(childAttrs()[CHILD_PATH]).toBe("child.fabro");
      expect(state.draft.assets["child.fabro"]).toBe(source);
    });
  });

  test("references added inline keep the parent folder when saved to a file", () => {
    mount(attachedDraft());
    switchMode("Inline source");
    const source = EDITED.replace("start [", 'graph [goal="@ticket.md"] start [');
    editInline(source);
    switchMode("Path");
    expect(childAttrs()[CHILD_PATH]).toBe("child.fabro");
    expect(state.draft.assets["child.fabro"]).toBe(source);
    expect(state.draft.assets["shared/check.fabro"]).toBe(ORIGINAL);
  });

  test("a failed conversion retains inline edits instead of switching modes", () => {
    mount(attachedDraft());
    switchMode("Inline source");
    const large = "x".repeat(MAX_ATTACHMENT_BYTES + 1);
    editInline(large);
    switchMode("Path");
    expect(childAttrs()[CHILD_SOURCE]).toBe(large);
    expect(childAttrs()[CHILD_PATH]).toBeUndefined();
    expect(renderer!.root.findByType("textarea").props.value).toBe(large);
    expect(state.draft.assets).toEqual({ "shared/check.fabro": ORIGINAL });
    expect(JSON.stringify(renderer!.toJSON())).toContain("too large for an attachment");
  });

  test("file allocation accounts for normalized aliases without replacing content", () => {
    const assets = { "./shared/check.fabro": ORIGINAL, "shared/check-2.fabro": "another child" };
    expect(freeAttachmentPath(assets, "shared/check.fabro", EDITED)).toBe("shared/check-3.fabro");
    expect(freeAttachmentPath(assets, "./shared/check.fabro", ORIGINAL)).toBe("shared/check.fabro");
    expect(assets["./shared/check.fabro"]).toBe(ORIGINAL);
  });
});
