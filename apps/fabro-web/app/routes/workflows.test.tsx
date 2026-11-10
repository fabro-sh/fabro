import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { createElement, Suspense, useEffect } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter, RouterProvider, createMemoryRouter, useLocation, useNavigate } from "react-router";

import { setupReactTestEnv, textContent } from "../lib/test-utils";
import { EXAMPLE_SOURCE } from "../lib/workflow-builder/example";
import { newDraft } from "../lib/workflow-builder/model";
import { listDrafts, saveDraft } from "../lib/workflow-builder/storage";
import type { WorkflowDraft } from "../lib/workflow-builder/types";
import { routes } from "../router";
import { navigation, createWorkflowHref } from "../layouts/navigation";

const backing = new Map<string, string>();
const previousStorage = globalThis.localStorage;
const KEY_PREFIX = "fabro.workflow-builder.v1.draft.";
let teardown: () => void;

// Headless UI's dialog needs a DOM. What matters here is whether it opens and what it confirms.
const confirmDialogProps: { open?: boolean; onConfirm?: () => void; onCancel?: () => void } = {};
const realUi = await import("../components/ui");
mock.module("../components/ui", () => ({
  ...realUi,
  ConfirmDialog: (props: typeof confirmDialogProps) => {
    Object.assign(confirmDialogProps, props);
    return null;
  },
}));

// The builder is a lazy chunk with a DOM-only canvas. The route's job is which draft it gets.
const builderMounts: { draftId: string; persisted: boolean }[] = [];
let lastBuilderProps: { initialDraft: WorkflowDraft; persisted: boolean; onFirstSave: (id: string) => void } | null = null;
mock.module("../components/workflow-builder/builder", () => ({
  default: function BuilderStub(props: NonNullable<typeof lastBuilderProps>) {
    lastBuilderProps = props;
    useEffect(() => {
      builderMounts.push({ draftId: props.initialDraft.id, persisted: props.persisted });
    }, []);
    return createElement("div", { "data-testid": "builder" }, props.initialDraft.name);
  },
}));

const { default: Workflows, handle: workflowsHandle } = await import("./workflows");
const { default: WorkflowBuilderRoute, handle: builderHandle } = await import("./workflow-builder");

beforeEach(() => {
  teardown = setupReactTestEnv();
  backing.clear();
  builderMounts.length = 0;
  lastBuilderProps = null;
  Object.assign(confirmDialogProps, { open: false, onConfirm: undefined, onCancel: undefined });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => backing.get(key) ?? null,
      setItem: (key: string, value: string) => void backing.set(key, value),
      removeItem: (key: string) => void backing.delete(key),
      key: (index: number) => [...backing.keys()][index] ?? null,
      get length() {
        return backing.size;
      },
    },
  });
});

afterEach(() => {
  teardown();
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: previousStorage });
});

function renderLibrary(): TestRenderer.ReactTestRenderer {
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={["/workflows"]}>
        <Workflows />
      </MemoryRouter>,
    );
  });
  if (!renderer) throw new Error("not rendered");
  return renderer;
}

function buttonNamed(renderer: TestRenderer.ReactTestRenderer, name: string) {
  const button = renderer.root.findAll(
    (node) => node.type === "button" && textContent(node).trim() === name,
  )[0];
  if (!button) throw new Error(`no button named ${name}`);
  return button;
}

function text(renderer: TestRenderer.ReactTestRenderer): string {
  return textContent(renderer.root);
}

describe("the workflow library", () => {
  test("says plainly where drafts live", () => {
    const renderer = renderLibrary();
    expect(text(renderer)).toContain("saved in this browser only");
  });

  test("an empty library offers a way in", () => {
    const renderer = renderLibrary();
    expect(text(renderer)).toContain("No workflows in this browser yet");
    expect(text(renderer)).toContain("Create workflow");
  });

  test("lists drafts with what's in them, newest first", () => {
    saveDraft({ ...newDraft("Older"), updatedAt: "2026-01-01T00:00:00.000Z" });
    saveDraft({ ...newDraft("Newer"), source: EXAMPLE_SOURCE, updatedAt: "2026-06-01T00:00:00.000Z" });
    const content = text(renderLibrary());
    expect(content.indexOf("Newer")).toBeLessThan(content.indexOf("Older"));
    expect(content).toContain("10 nodes · 12 edges · 5 stages");
    expect(content).toContain("3 nodes · 2 edges");
  });

  test("flags a draft whose code doesn't parse, without touching it", () => {
    const source = "digraph Broken {\n  start [";
    saveDraft({ ...newDraft("Broken"), source });
    expect(text(renderLibrary())).toContain("Code has errors");
    expect(listDrafts()[0].source).toBe(source);
  });

  test("renames a draft and keeps its content", () => {
    const draft = newDraft("Before");
    saveDraft(draft);
    const renderer = renderLibrary();
    act(() => {
      buttonNamed(renderer, "Rename").props.onClick();
    });
    const input = renderer.root.findByType("form").findByType("input");
    act(() => {
      input.props.onChange({ target: { value: "After" } });
    });
    const form = renderer.root.findByType("form");
    act(() => {
      form.props.onSubmit({ preventDefault: () => undefined });
    });
    const [saved] = listDrafts();
    expect(saved.name).toBe("After");
    expect(saved.source).toBe(draft.source);
    expect(saved.id).toBe(draft.id);
    expect(text(renderer)).toContain("After");
  });

  test("deletes only after confirming", () => {
    saveDraft(newDraft("Doomed"));
    const renderer = renderLibrary();
    act(() => {
      buttonNamed(renderer, "Delete").props.onClick();
    });
    expect(confirmDialogProps.open).toBe(true);
    expect(listDrafts()).toHaveLength(1);

    act(() => {
      confirmDialogProps.onConfirm?.();
    });
    expect(listDrafts()).toHaveLength(0);
    expect(text(renderer)).not.toContain("Doomed");
  });

  test("cancelling a delete keeps the draft", () => {
    saveDraft(newDraft("Spared"));
    const renderer = renderLibrary();
    act(() => {
      buttonNamed(renderer, "Delete").props.onClick();
    });
    act(() => {
      confirmDialogProps.onCancel?.();
    });
    expect(listDrafts()).toHaveLength(1);
  });

  test("a damaged entry is reported and left alone, and good drafts still list", () => {
    saveDraft(newDraft("Healthy"));
    backing.set(`${KEY_PREFIX}bad`, "{not json");
    const content = text(renderLibrary());
    expect(content).toContain("Healthy");
    expect(content).toContain("couldn't be read");
    expect(content).toContain("Download raw data");
    expect(backing.get(`${KEY_PREFIX}bad`)).toBe("{not json");
  });

  test("unavailable storage is an error, not an empty list", () => {
    Object.defineProperty(globalThis, "localStorage", { configurable: true, value: undefined });
    const content = text(renderLibrary());
    expect(content).toContain("storage is unavailable");
    expect(content).not.toContain("No workflows in this browser yet");
  });

  test("the page handle puts Create in the header", () => {
    expect(workflowsHandle.headerExtra).toBeDefined();
  });
});

describe("opening a draft in the builder", () => {
  function Probe({ onReady }: { onReady: (api: { navigate: ReturnType<typeof useNavigate>; pathname: string }) => void }) {
    const navigate = useNavigate();
    const { pathname } = useLocation();
    onReady({ navigate, pathname });
    return null;
  }

  async function renderBuilderAt(path: string) {
    let api: { navigate: ReturnType<typeof useNavigate>; pathname: string } | undefined;
    const router = createMemoryRouter(
      [
        {
          path: "/workflows/:id",
          Component: function Wrapped() {
            const { pathname } = useLocation();
            const id = pathname.split("/").pop() ?? "";
            return (
              <>
                <Probe onReady={(next) => (api = next)} />
                <Suspense fallback="loading">
                  <WorkflowBuilderRoute params={{ id }} />
                </Suspense>
              </>
            );
          },
        },
        { path: "/elsewhere", Component: () => null },
      ],
      { initialEntries: [path] },
    );
    let renderer: TestRenderer.ReactTestRenderer | undefined;
    await act(async () => {
      renderer = TestRenderer.create(<RouterProvider router={router} />);
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    if (!renderer || !api) throw new Error("not rendered");
    return {
      renderer,
      get api() {
        return api as NonNullable<typeof api>;
      },
    };
  }

  test("takes the whole viewport", () => {
    expect(builderHandle).toEqual({ hideHeader: true, fullHeight: true, wide: true });
  });

  test("Create starts a clean draft that isn't saved until it is edited", async () => {
    await renderBuilderAt("/workflows/new");
    expect(lastBuilderProps?.persisted).toBe(false);
    expect(lastBuilderProps?.initialDraft.source).toContain("Mdiamond");
    expect(backing.size).toBe(0);
  });

  test("the first save swaps the URL without remounting the editor", async () => {
    const page = await renderBuilderAt("/workflows/new");
    const draft = lastBuilderProps?.initialDraft;
    if (!draft) throw new Error("no draft");
    saveDraft(draft);

    await act(async () => {
      lastBuilderProps?.onFirstSave(draft.id);
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    expect(page.api.pathname).toBe(`/workflows/${draft.id}`);
    expect(builderMounts.filter((mount) => mount.draftId === draft.id)).toHaveLength(1);
    expect(lastBuilderProps?.initialDraft.id).toBe(draft.id);
  });

  test("Create while on a new draft starts another new one", async () => {
    const page = await renderBuilderAt("/workflows/new");
    const first = lastBuilderProps?.initialDraft.id;
    await act(async () => {
      page.api.navigate(createWorkflowHref);
      await new Promise((resolve) => setTimeout(resolve, 10));
    });
    expect(lastBuilderProps?.initialDraft.id).not.toBe(first);
  });

  test("reopening a saved draft loads what was stored", async () => {
    const draft = { ...newDraft("Stored"), positions: { start: { x: 4, y: 8 }, agent: { x: 12, y: 16 }, exit: { x: 20, y: 24 } } };
    saveDraft(draft);
    await renderBuilderAt(`/workflows/${draft.id}`);
    expect(lastBuilderProps?.persisted).toBe(true);
    expect(lastBuilderProps?.initialDraft).toEqual(draft);
  });

  test("a draft that isn't here says so, and offers the way back", async () => {
    const { renderer } = await renderBuilderAt("/workflows/not-a-real-id");
    expect(textContent(renderer.root)).toContain("isn't in this browser");
    expect(textContent(renderer.root)).toContain("Back to workflows");
  });

  test("a damaged draft says why instead of opening blank", async () => {
    backing.set(`${KEY_PREFIX}damaged`, "{nope");
    const { renderer } = await renderBuilderAt("/workflows/damaged");
    expect(textContent(renderer.root)).toContain("couldn't be opened");
    expect(backing.get(`${KEY_PREFIX}damaged`)).toBe("{nope");
  });
});

describe("navigation", () => {
  function collectPaths(objects: typeof routes, prefix = ""): string[] {
    return objects.flatMap((route) => {
      const path = route.path ? `${prefix}/${route.path}`.replace(/\/+/g, "/") : prefix;
      return [path, ...(route.children ? collectPaths(route.children, path) : [])];
    });
  }

  test("the library and the builder are routes", () => {
    const paths = collectPaths(routes);
    expect(paths).toContain("/workflows");
    expect(paths).toContain("/workflows/:id");
  });

  test("every destination says what it's for", () => {
    for (const item of navigation) expect(item.purpose.length).toBeGreaterThan(10);
  });

  test("Workflows is in the navigation and Create points at a new draft", () => {
    expect(navigation.map((item) => item.href)).toContain("/workflows");
    expect(createWorkflowHref).toBe("/workflows/new");
  });
});
