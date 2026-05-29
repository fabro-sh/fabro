import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { createElement } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { createMemoryRouter, RouterProvider } from "react-router";

import { ToastProvider } from "../components/toast";
import { setupReactTestEnv } from "../lib/test-utils";

let currentRun: any = null;
let currentRunError: unknown = null;
let currentRunLoading = false;
let currentRunSettings: any = null;
const queryCalls: Array<{ hook: string; id: string | undefined }> = [];
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];
let teardownReactEnv: (() => void) | undefined;

const createAutomationMock = mock((_payload: unknown) =>
  Promise.resolve({ data: {} }),
);
const swrMutateMock = mock((_key: unknown) => Promise.resolve(undefined));

mock.module("@headlessui/react", () => ({
  Dialog: ({ open, children }: any) =>
    open ? createElement("div", { role: "dialog" }, children) : null,
  DialogPanel: ({ children, ...props }: any) =>
    createElement("div", props, children),
  DialogTitle: ({ children, ...props }: any) =>
    createElement("h2", props, children),
  Switch: ({ checked, onChange, children, ...props }: any) =>
    createElement(
      "button",
      {
        ...props,
        type:           "button",
        role:           "switch",
        "aria-checked": checked,
        onClick:        () => onChange(!checked),
      },
      children,
    ),
}));

mock.module("../lib/queries", () => ({
  useRun: (id: string | undefined) => {
    queryCalls.push({ hook: "useRun", id });
    return {
      data:      currentRun,
      error:     currentRunError,
      isLoading: currentRunLoading,
    };
  },
  useRunSettings: (id: string | undefined) => {
    queryCalls.push({ hook: "useRunSettings", id });
    return {
      data:      currentRunSettings,
      error:     null,
      isLoading: false,
    };
  },
}));

mock.module("../lib/api-client", () => ({
  ApiError: class ApiError extends Error {
    readonly status: number;
    readonly requestId: string | null;
    readonly body: unknown;

    constructor({
      status,
      message,
      requestId,
      body,
    }: {
      status: number;
      message: string;
      requestId: string | null;
      body: unknown;
    }) {
      super(message);
      this.name = "ApiError";
      this.status = status;
      this.requestId = requestId;
      this.body = body;
    }
  },
  apiData: async function apiData<T>(
    call: () => Promise<{ data: T }>,
  ): Promise<T> {
    const response = await call();
    return response.data;
  },
  automationsApi: {
    createAutomation: createAutomationMock,
  },
}));

mock.module("swr", () => ({
  useSWRConfig: () => ({ mutate: swrMutateMock }),
}));

const { default: AutomationsNew } = await import("./automations-new");
mock.restore();

function makeRun(overrides: Record<string, unknown> = {}) {
  return {
    id:               "run_1",
    children_count:   0,
    goal:             "Fix CI",
    title:            "Fix failing tests",
    workflow:         {
      slug:       "fix_ci",
      name:       "Fix CI",
      graph_name: "ci_graph",
      node_count: 0,
      edge_count: 0,
    },
    automation:       null,
    repository:       {
      name:       "fallback/repo",
      origin_url: "https://github.com/fallback/repo.git",
      provider:   "github",
    },
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
    sandbox:          {
      kind:     "ready",
      plan:     { provider: "docker", image: null, snapshot: null },
      instance: {
        provider: "docker",
        image:    null,
        snapshot: null,
        runtime:  {
          id:                "container_1",
          working_directory: "/workspace",
          repo_cloned:       true,
          clone_origin_url:  "https://github.com/qltysh/fabro.git",
          clone_branch:      "feature/from-run",
        },
      },
    },
    models:           [],
    source_directory: null,
    timestamps:       {
      created_at:     "2026-04-20T12:00:00Z",
      started_at:     null,
      last_event_at:  null,
      completed_at:   null,
    },
    timing:           null,
    billing:          null,
    size:             "XS",
    ask_fabro:        {
      available:          false,
      unavailable_reason: "no_sandbox",
      default_model:      null,
    },
    diff:             null,
    pull_request:     null,
    current_question: null,
    superseded_by:    null,
    retried_from:     null,
    links:            { web: null },
    ...overrides,
  };
}

function makeRunSettings() {
  return {
    run: {
      scm: {
        provider:   "github",
        owner:      "qltysh",
        repository: "fabro",
        github:     null,
      },
    },
  };
}

async function renderAutomationsNew(initialEntry: string) {
  const router = createMemoryRouter(
    [
      {
        path:    "/automations",
        element: <div data-route="automations">Automations</div>,
      },
      {
        path:    "/automations/new",
        element: <AutomationsNew />,
      },
    ],
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

function fieldValue(renderer: TestRenderer.ReactTestRenderer, label: string) {
  return renderer.root.findByProps({ "aria-label": label }).props.value;
}

function switchChecked(renderer: TestRenderer.ReactTestRenderer, label: string) {
  const props = renderer.root.findByProps({ "aria-label": label }).props;
  return props["aria-checked"] ?? props.checked;
}

function textFromNode(
  node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>,
): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textFromNode).join(" ");
  return (node.children ?? []).map(textFromNode).join(" ");
}

beforeEach(() => {
  teardownReactEnv = setupReactTestEnv();
  currentRun = null;
  currentRunError = null;
  currentRunLoading = false;
  currentRunSettings = null;
  queryCalls.length = 0;
  createAutomationMock.mockClear();
  swrMutateMock.mockClear();
});

afterEach(() => {
  for (const renderer of mountedRenderers.splice(0)) {
    act(() => renderer.unmount());
  }
  teardownReactEnv?.();
  teardownReactEnv = undefined;
});

describe("AutomationsNew", () => {
  test("/automations/new renders empty form values", async () => {
    const { renderer } = await renderAutomationsNew("/automations/new");

    expect(fieldValue(renderer, "Automation name")).toBe("");
    expect(fieldValue(renderer, "Automation slug")).toBe("");
    expect(fieldValue(renderer, "Repository")).toBe("");
    expect(fieldValue(renderer, "Default branch")).toBe("main");
    expect(fieldValue(renderer, "Workflow slug")).toBe("");
    expect(switchChecked(renderer, "Enable manual and API triggers")).toBe(true);
    expect(switchChecked(renderer, "Enable scheduled triggers")).toBe(false);
  });

  test("/automations/new?from_run=run_1 pre-populates from run and settings data", async () => {
    currentRun = makeRun();
    currentRunSettings = makeRunSettings();

    const { renderer } = await renderAutomationsNew("/automations/new?from_run=run_1");

    expect(fieldValue(renderer, "Automation name")).toBe("Fix failing tests");
    expect(fieldValue(renderer, "Automation slug")).toBe("fix-failing-tests");
    expect(fieldValue(renderer, "Repository")).toBe("qltysh/fabro");
    expect(fieldValue(renderer, "Default branch")).toBe("feature/from-run");
    expect(fieldValue(renderer, "Workflow slug")).toBe("fix_ci");
    expect(switchChecked(renderer, "Enable manual and API triggers")).toBe(true);
    expect(switchChecked(renderer, "Enable scheduled triggers")).toBe(false);
    expect(
      renderer.root.findAllByProps({ "aria-label": "Cron expression" }),
    ).toHaveLength(0);
    expect(queryCalls).toContainEqual({ hook: "useRun", id: "run_1" });
    expect(queryCalls).toContainEqual({ hook: "useRunSettings", id: "run_1" });
  });

  test("missing source run data renders an editable empty form with a non-blocking error", async () => {
    currentRun = null;
    currentRunError = new Error("not found");

    const { renderer } = await renderAutomationsNew("/automations/new?from_run=run_1");

    expect(textFromNode(renderer.toJSON())).toContain("could not be loaded");
    expect(textFromNode(renderer.toJSON())).toContain("fill it out manually");
    expect(fieldValue(renderer, "Automation name")).toBe("");
    expect(fieldValue(renderer, "Repository")).toBe("");
    expect(fieldValue(renderer, "Default branch")).toBe("main");
    expect(fieldValue(renderer, "Workflow slug")).toBe("");
  });
});
