import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { createElement } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { createMemoryRouter, RouterProvider } from "react-router";

import { ToastProvider } from "../components/toast";
import { TEST_PRINCIPAL, makeUsage } from "../lib/test-fixtures";
import { setupReactTestEnv } from "../lib/test-utils";

let currentRun: any = null;
let currentRunError: unknown = null;
let currentRunLoading = false;
let currentRunState: any = null;
let currentRunStateLoading = false;
let currentRunSettings: any = null;
let currentEnvironments: any[] = [];
let currentEnvironmentsError: unknown = null;
let currentGithubWebUrl = "https://github.com";
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
  useEnvironments: () => ({
    data:      { data: currentEnvironments, meta: { total: currentEnvironments.length } },
    error:     currentEnvironmentsError,
    isLoading: false,
  }),
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
  useServerSettings: () => ({
    data:      { server: { integrations: { github: { web_url: currentGithubWebUrl } } } },
    error:     null,
    isLoading: false,
  }),
  useRunState: (id: string | undefined) => {
    queryCalls.push({ hook: "useRunState", id });
    return {
      data:      currentRunState,
      error:     null,
      isLoading: currentRunStateLoading,
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
const { automationFormValuesFromRun } = await import("../components/automation-form");
mock.restore();

function makeRun(overrides: Record<string, unknown> = {}) {
  return {
    id:               "run_1",
    children_count:   0,
    goal:             "Fix CI",
    title:            "Fix failing tests",
    workflow:         {
      slug:       "fix-ci",
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
    created_by:       TEST_PRINCIPAL,
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
    usage:          makeUsage(),
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
      environment: {
        id:       "default",
        provider: "docker",
      },
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

function byLabel(renderer: TestRenderer.ReactTestRenderer, label: string) {
  return renderer.root.findByProps({ "aria-label": label });
}

function fieldValue(renderer: TestRenderer.ReactTestRenderer, label: string) {
  return byLabel(renderer, label).props.value;
}

function changeField(
  renderer: TestRenderer.ReactTestRenderer,
  label: string,
  value: string,
) {
  act(() => {
    byLabel(renderer, label).props.onChange({ target: { value } });
  });
}

function switchChecked(renderer: TestRenderer.ReactTestRenderer, label: string) {
  const props = byLabel(renderer, label).props;
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
  currentRunState = null;
  currentRunStateLoading = false;
  currentRunSettings = null;
  currentGithubWebUrl = "https://github.com";
  currentEnvironments = [
    { id: "default", provider: "docker" },
    { id: "daytona-smoke", provider: "daytona" },
    { id: "local", provider: "local" },
  ];
  currentEnvironmentsError = null;
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
    expect(fieldValue(renderer, "Run target repository")).toBe("");
    expect(fieldValue(renderer, "Working branch")).toBe("main");
    expect(fieldValue(renderer, "Tag")).toBe("");
    expect(fieldValue(renderer, "Exact commit SHA")).toBe("");
    expect(fieldValue(renderer, "Workflow slug")).toBe("");
    expect(fieldValue(renderer, "Automation environment")).toBe("");
    expect(switchChecked(renderer, "Enable manual and API triggers")).toBe(true);
    expect(switchChecked(renderer, "Enable scheduled triggers")).toBe(false);
    expect(switchChecked(renderer, "Use a remote workflow")).toBe(false);
    expect(renderer.root.findAllByProps({ "aria-label": "Remote workflow repository" })).toHaveLength(0);
  });

  test("environment selector offers Docker and Daytona but not local", async () => {
    const { renderer } = await renderAutomationsNew("/automations/new");
    const options = byLabel(renderer, "Automation environment").findAllByType("option");
    const optionText = options.map((option) =>
      option.children.join("").replace(/\s+/g, " ").trim(),
    );

    expect(optionText).toContain("default · Docker");
    expect(optionText).toContain("daytona-smoke · Daytona");
    expect(optionText.some((text) => text.includes("local"))).toBe(false);
  });

  test("no compatible environments explains recovery and blocks creation", async () => {
    currentEnvironments = [{ id: "local", provider: "local" }];

    const { renderer } = await renderAutomationsNew("/automations/new");

    expect(textFromNode(renderer.toJSON())).toContain(
      "No Docker or Daytona environments are available",
    );
    expect(
      renderer.root.findByProps({ type: "submit" }).props.disabled,
    ).toBe(true);
  });

  test("creation sends the selected environment id", async () => {
    const { renderer } = await renderAutomationsNew("/automations/new");
    changeField(renderer, "Automation name", "Nightly");
    changeField(renderer, "Run target repository", "fabro-sh/fabro");
    changeField(renderer, "Workflow slug", "hello");
    changeField(renderer, "Automation environment", "daytona-smoke");

    await act(async () => {
      await renderer.root.findByType("form").props.onSubmit({ preventDefault() {} });
    });

    expect(createAutomationMock).toHaveBeenCalledTimes(1);
    expect(createAutomationMock.mock.calls[0]?.[0]).toMatchObject({
      environment_id: "daytona-smoke",
    });
  });

  test("workflow slug input normalizes to kebab-case and preserves dashes", async () => {
    const { renderer } = await renderAutomationsNew("/automations/new");

    changeField(renderer, "Workflow slug", "patch-cves");
    expect(fieldValue(renderer, "Workflow slug")).toBe("patch-cves");

    changeField(renderer, "Workflow slug", "Patch CVEs");
    expect(fieldValue(renderer, "Workflow slug")).toBe("patch-cves");
  });

  test("/automations/new?from_run=run_1 pre-populates from run and settings data", async () => {
    currentRun = makeRun();
    currentRunSettings = makeRunSettings();

    const { renderer } = await renderAutomationsNew("/automations/new?from_run=run_1");

    expect(fieldValue(renderer, "Automation name")).toBe("Fix failing tests");
    expect(fieldValue(renderer, "Automation slug")).toBe("fix-failing-tests");
    expect(fieldValue(renderer, "Run target repository")).toBe("qltysh/fabro");
    expect(fieldValue(renderer, "Working branch")).toBe("feature/from-run");
    expect(fieldValue(renderer, "Tag")).toBe("");
    expect(fieldValue(renderer, "Exact commit SHA")).toBe("");
    expect(fieldValue(renderer, "Workflow slug")).toBe("fix-ci");
    expect(fieldValue(renderer, "Automation environment")).toBe("default");
    expect(switchChecked(renderer, "Enable manual and API triggers")).toBe(true);
    expect(switchChecked(renderer, "Enable scheduled triggers")).toBe(false);
    expect(switchChecked(renderer, "Use a remote workflow")).toBe(false);
    expect(
      renderer.root.findAllByProps({ "aria-label": "Cron expression" }),
    ).toHaveLength(0);
    expect(queryCalls).toContainEqual({ hook: "useRun", id: "run_1" });
    expect(queryCalls).toContainEqual({ hook: "useRunState", id: "run_1" });
    expect(queryCalls).toContainEqual({ hook: "useRunSettings", id: "run_1" });
  });

  test("canonical run target wins over legacy run, settings, and sandbox projections", async () => {
    currentRun = makeRun();
    currentRunSettings = makeRunSettings();
    currentRunState = {
      spec: {
        target: {
          kind:   "git",
          repo:   "canonical/repo",
          branch: "release",
          tag:    "v2.0.0",
          sha:    "0123456789abcdef0123456789abcdef01234567",
        },
      },
    };

    const { renderer } = await renderAutomationsNew("/automations/new?from_run=run_1");

    expect(fieldValue(renderer, "Run target repository")).toBe("canonical/repo");
    expect(fieldValue(renderer, "Working branch")).toBe("release");
    expect(fieldValue(renderer, "Tag")).toBe("v2.0.0");
    expect(fieldValue(renderer, "Exact commit SHA")).toBe(
      "0123456789abcdef0123456789abcdef01234567",
    );
  });

  test("run origin on the configured GitHub Enterprise host prefills the target repository", async () => {
    currentGithubWebUrl = "https://ghe.example.com";
    currentRun = makeRun({
      repository: {
        name:       "",
        origin_url: "https://ghe.example.com/acme/widgets.git",
        provider:   "github",
      },
    });

    const { renderer } = await renderAutomationsNew("/automations/new?from_run=run_1");

    expect(fieldValue(renderer, "Run target repository")).toBe("acme/widgets");
  });

  test("automationFormValuesFromRun kebab-cases the workflow name fallback", () => {
    const run = makeRun({
      workflow: {
        slug:       "",
        name:       "Patch CVEs",
        graph_name: "PatchCves",
        node_count: 0,
        edge_count: 0,
      },
    });

    expect(automationFormValuesFromRun(run as any).workflow).toBe("patch-cves");
  });

  test("missing source run data renders an editable empty form with a non-blocking error", async () => {
    currentRun = null;
    currentRunError = new Error("not found");

    const { renderer } = await renderAutomationsNew("/automations/new?from_run=run_1");

    expect(textFromNode(renderer.toJSON())).toContain("could not be loaded");
    expect(textFromNode(renderer.toJSON())).toContain("fill it out manually");
    expect(fieldValue(renderer, "Automation name")).toBe("");
    expect(fieldValue(renderer, "Run target repository")).toBe("");
    expect(fieldValue(renderer, "Working branch")).toBe("main");
    expect(fieldValue(renderer, "Workflow slug")).toBe("");
  });

  test("submits a canonical explicit workflow source", async () => {
    const { renderer } = await renderAutomationsNew("/automations/new");
    changeField(renderer, "Automation name", "Nightly");
    changeField(renderer, "Run target repository", "fabro-sh/app");
    changeField(renderer, "Automation environment", "daytona-smoke");
    changeField(renderer, "Workflow slug", "release");
    act(() => {
      byLabel(renderer, "Use a remote workflow").props.onChange(true);
    });
    changeField(renderer, "Remote workflow repository", " fabro-sh/workflows ");
    changeField(renderer, "Remote workflow branch", " release ");
    changeField(renderer, "Remote workflow tag", " v2.0.0 ");
    changeField(
      renderer,
      "Remote workflow exact commit SHA",
      "ABCDEF0123456789ABCDEF0123456789ABCDEF01",
    );

    await act(async () => {
      await renderer.root.findByType("form").props.onSubmit({ preventDefault() {} });
    });

    expect(createAutomationMock).toHaveBeenCalledTimes(1);
    expect(createAutomationMock.mock.calls[0]?.[0]).toMatchObject({
      workflow_source: {
        repo: "fabro-sh/workflows",
        branch: "release",
        tag: "v2.0.0",
        sha: "abcdef0123456789abcdef0123456789abcdef01",
      },
    });
  });
});
