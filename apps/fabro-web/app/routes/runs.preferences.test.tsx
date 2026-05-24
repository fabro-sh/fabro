import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { createMemoryRouter, RouterProvider } from "react-router";
import type { PaginatedRunList, Run } from "@qltysh/fabro-api-client";

import { ToastProvider } from "../components/toast";
import { setupReactTestEnv } from "../lib/test-utils";

class MemoryStorage {
  values = new Map<string, string>();

  getItem(key: string) {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string) {
    this.values.set(key, value);
  }
}

let storage: MemoryStorage;
let teardownReactEnv: (() => void) | undefined;
let previousWindow: unknown;
let hadWindow = false;
let previousElement: unknown;
let hadElement = false;
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function run(id: string, repo = "qlty/fabro", workflow = "release"): Run {
  return {
    id,
    goal:             `Run ${id}`,
    title:            `Run ${id}`,
    workflow:         { slug: workflow, name: workflow, graph_name: null, node_count: 0, edge_count: 0 },
    automation:       null,
    repository:       { name: repo, origin_url: null, provider: "github" },
    created_by:       null,
    origin:           { kind: "api" },
    labels:           {},
    lifecycle:        {
      status:          { kind: "succeeded", reason: "completed" },
      approval:        null,
      pending_control: null,
      queue_position:  null,
      error:           null,
      archived:        false,
      archived_at:     null,
    },
    sandbox:          null,
    models:           [],
    source_directory: null,
    timestamps:       {
      created_at:     "2026-04-19T12:00:00Z",
      started_at:     "2026-04-19T12:01:00Z",
      last_event_at:  "2026-04-19T12:04:00Z",
      completed_at:   "2026-04-19T12:05:00Z",
    },
    billing:          null,
    size:             "XS",
    diff:             null,
    pull_request:     null,
    current_question: null,
    superseded_by:    null,
    retried_from:     null,
    links:            { web: null },
  };
}

const allRuns = { data: [run("run-1"), run("run-2", "qlty/docs", "docs")] };
const pageRuns: PaginatedRunList = {
  data: [run("run-1")],
  meta: { has_more: false, total: 1 },
};

const queryCalls: Array<{ hook: string; args: unknown[] }> = [];

mock.module("../lib/queries", () => ({
  useAllRuns: (...args: unknown[]) => {
    queryCalls.push({ hook: "useAllRuns", args });
    return { data: allRuns, isLoading: false };
  },
  useRunsPage: (...args: unknown[]) => {
    queryCalls.push({ hook: "useRunsPage", args });
    return { data: pageRuns, isLoading: false };
  },
  useAuthConfig: () => ({ data: { methods: ["github"] } }),
  useSystemInfo: () => ({ data: { server_url: "http://127.0.0.1:32276" } }),
}));

mock.module("../lib/board-events", () => ({
  shouldRefreshBoardForEvent: () => false,
  useBoardEvents: () => {},
}));

mock.module("swr", () => ({
  useSWRConfig: () => ({ mutate: () => Promise.resolve(undefined) }),
}));

const {
  default: Runs,
  RUNS_PREFERENCES_STORAGE_KEY,
} = await import("./runs");

function installWindow() {
  class TestElement {}
  const globals = globalThis as { Element?: unknown; window?: unknown };
  hadWindow = "window" in globals;
  previousWindow = globals.window;
  hadElement = "Element" in globals;
  previousElement = globals.Element;
  globals.Element = TestElement;
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value:        { Element: TestElement, localStorage: storage },
  });
}

function restoreWindow() {
  const globals = globalThis as { Element?: unknown; window?: unknown };
  if (hadWindow) {
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value:        previousWindow,
    });
  } else {
    delete globals.window;
  }
  if (hadElement) {
    globals.Element = previousElement;
  } else {
    delete globals.Element;
  }
}

async function renderRuns(initialEntry: string) {
  const router = createMemoryRouter(
    [{ path: "/runs", element: <Runs /> }],
    { initialEntries: [initialEntry] },
  );
  let renderer!: TestRenderer.ReactTestRenderer;
  await act(async () => {
    renderer = TestRenderer.create(
      <ToastProvider>
        <RouterProvider router={router} />
      </ToastProvider>,
    );
  });
  mountedRenderers.push(renderer);
  return { renderer, router };
}

async function flushEffects() {
  await act(async () => {});
}

function buttonByLabel(renderer: TestRenderer.ReactTestRenderer, label: string) {
  return renderer.root.findByProps({ "aria-label": label });
}

function compositeByName(
  renderer: TestRenderer.ReactTestRenderer,
  name: string,
  predicate: (props: Record<string, unknown>) => boolean = () => true,
) {
  return renderer.root.findAll(
    (node) =>
      typeof node.type === "function" &&
      node.type.name === name &&
      predicate(node.props),
  )[0];
}

beforeEach(() => {
  storage = new MemoryStorage();
  teardownReactEnv = setupReactTestEnv();
  installWindow();
  queryCalls.length = 0;
});

afterEach(() => {
  for (const renderer of mountedRenderers.splice(0)) {
    act(() => renderer.unmount());
  }
  restoreWindow();
  teardownReactEnv?.();
  teardownReactEnv = undefined;
});

describe("Runs workspace preference restoration", () => {
  test("/runs with stored list view renders list mode", async () => {
    storage.setItem(
      RUNS_PREFERENCES_STORAGE_KEY,
      JSON.stringify({ version: 1, view: "list" }),
    );

    const { renderer, router } = await renderRuns("/runs");
    await flushEffects();

    expect(router.state.location.search).toBe("?view=list");
    expect(buttonByLabel(renderer, "List view").props["aria-pressed"]).toBe(true);
  });

  test("/runs applies stored list+archived prefs on the first render (no Quick Start flash)", async () => {
    storage.setItem(
      RUNS_PREFERENCES_STORAGE_KEY,
      JSON.stringify({ version: 1, view: "list", archived: true }),
    );

    await renderRuns("/runs");

    // The first frame the user sees must already reflect stored prefs.
    // Before this was fixed, the route briefly rendered the columns view
    // with includeArchived=false (default state) before a post-commit
    // useEffect restored the URL, flashing the Quick Start empty state for
    // users whose only runs were archived.
    const firstAllRuns = queryCalls.find((c) => c.hook === "useAllRuns");
    const firstRunsPage = queryCalls.find((c) => c.hook === "useRunsPage");
    expect(firstAllRuns?.args).toEqual([{ includeArchived: true }, false]);
    expect(firstRunsPage?.args[0]).toMatchObject({ includeArchived: true });
    expect(firstRunsPage?.args[1]).toBe(true);
  });

  test("/runs?view=columns ignores stored list view", async () => {
    storage.setItem(
      RUNS_PREFERENCES_STORAGE_KEY,
      JSON.stringify({ version: 1, view: "list" }),
    );

    const { renderer, router } = await renderRuns("/runs?view=columns");
    await flushEffects();

    expect(router.state.location.search).toBe("?view=columns");
    expect(buttonByLabel(renderer, "Columns view").props["aria-pressed"]).toBe(true);
  });

  test("switching from list to columns stores columns while removing view from the URL", async () => {
    const { renderer, router } = await renderRuns("/runs?view=list");

    await act(async () => {
      buttonByLabel(renderer, "Columns view").props.onClick();
    });

    expect(router.state.location.search).toBe("");
    expect(JSON.parse(storage.getItem(RUNS_PREFERENCES_STORAGE_KEY) ?? "{}").view).toBe("columns");
  });

  test("clicking a sort header in list view updates the URL while preserving other params", async () => {
    const { renderer, router } = await renderRuns("/runs?view=list&archived=1");

    await act(async () => {
      compositeByName(renderer, "SortHeader", (props) => props.sortKey === "status").props.onClick("status");
    });

    expect(router.state.location.search).toContain("sort=status");
    expect(router.state.location.search).toContain("view=list");
    expect(router.state.location.search).toContain("archived=1");

    // Clicking the same header again toggles direction to ascending.
    await act(async () => {
      compositeByName(renderer, "SortHeader", (props) => props.sortKey === "status").props.onClick("status");
    });

    expect(router.state.location.search).toContain("sort=status");
    expect(router.state.location.search).toContain("direction=asc");
    expect(router.state.location.search).toContain("view=list");
    expect(router.state.location.search).toContain("archived=1");
  });

  test("changing filters and hidden columns persists them", async () => {
    const { renderer } = await renderRuns("/runs?view=list");

    await act(async () => {
      renderer.root.findByProps({ "aria-label": "Search runs" }).props.onChange({
        target: { value: "release fix" },
      });
    });
    await act(async () => {
      compositeByName(renderer, "FilterButton", (props) => props.label === "Repo").props.onChange("qlty/docs");
    });
    await act(async () => {
      compositeByName(renderer, "FilterButton", (props) => props.label === "Workflow").props.onChange("docs");
    });
    await act(async () => {
      compositeByName(renderer, "FilterButton", (props) => props.label === "Time").props.onChange("7d");
    });
    await act(async () => {
      renderer.root.findByProps({ title: "Show archived runs" }).props.onClick();
    });
    await act(async () => {
      compositeByName(renderer, "ColumnPickerButton").props.onChange(new Set(["repo", "workflow"]));
    });

    expect(JSON.parse(storage.getItem(RUNS_PREFERENCES_STORAGE_KEY) ?? "{}")).toMatchObject({
      view:     "list",
      search:   "release fix",
      repo:     "qlty/docs",
      workflow: "docs",
      created:  "7d",
      archived: true,
      hide:     "repo,workflow",
    });
  });
});
