import { afterEach, describe, expect, mock, test } from "bun:test";
import { createElement } from "react";
import TestRenderer, { act } from "react-test-renderer";
import {
  createMemoryRouter,
  RouterProvider,
  useParams,
} from "react-router";
import {
  AskFabroUnavailableReasonEnum,
  QuestionType,
} from "@qltysh/fabro-api-client";

import { ToastProvider } from "../components/toast";
import { DemoModeProvider } from "../lib/demo-mode";

let currentRunSummary: any = null;
let currentRunState: any = null;
let currentQuestions: any[] = [];
let deleteRunApiResult: Promise<unknown> | null = null;
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

const deleteRunApiMock = mock((_id: string) =>
  deleteRunApiResult ?? Promise.resolve({}),
);
const mutateRunListCachesMock = mock((_mutate: unknown) => undefined);
const swrMutateMock = mock((_key: unknown) => Promise.resolve(undefined));

mock.module("@headlessui/react", () => ({
  Dialog: ({ open, children }: any) =>
    open ? createElement("div", { role: "dialog" }, children) : null,
  DialogPanel: ({ children, ...props }: any) =>
    createElement("div", props, children),
  DialogTitle: ({ children, ...props }: any) =>
    createElement("h2", props, children),
  Menu: ({ as: Component = "div", children, ...props }: any) =>
    createElement(Component, props, children),
  MenuButton: ({ children, onClick, ...props }: any) =>
    createElement("button", { ...props, onClick: onClick ?? (() => undefined) }, children),
  MenuItem: ({ children }: any) =>
    typeof children === "function"
      ? children({ close: () => undefined })
      : children,
  MenuItems: ({ children, anchor: _anchor, transition: _transition, ...props }: any) =>
    createElement("div", props, children),
}));

mock.module("../lib/queries", () => ({
  useRun: () => ({
    data:      currentRunSummary,
    isLoading: false,
  }),
  useRunQuestions: () => ({
    data: currentQuestions,
  }),
  useRunPullRequest: () => ({
    data:      null,
    isLoading: false,
  }),
  useRunState: () => ({
    data: currentRunState,
  }),
  useRunFiles: () => ({
    data:         null,
    error:        null,
    isLoading:    false,
    isValidating: false,
    mutate:       mock(() => Promise.resolve(null)),
  }),
}));

mock.module("../lib/run-events", () => ({
  useRunEvents: () => undefined,
}));

mock.module("../hooks/use-run-toasts", () => ({
  useRunToasts: () => undefined,
}));

mock.module("../lib/api-client", () => ({
  apiData: async function apiData<T>(
    call: () => Promise<{ data: T }>,
  ): Promise<T> {
    const response = await call();
    return response.data;
  },
  apiResponse: async function apiResponse<T>(call: () => Promise<T>): Promise<T> {
    return await call();
  },
  requestSignalOptions: () => undefined,
  runsApi: {
    deleteRun: deleteRunApiMock,
  },
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
}));

mock.module("../lib/board-cache", () => ({
  mutateRunListCaches: mutateRunListCachesMock,
}));

mock.module("swr", () => ({
  useSWRConfig: () => ({ mutate: swrMutateMock }),
}));

mock.module("../components/chats/ask-fabro-sidebar", () => ({
  SIDEBAR_WIDTH: 420,
  default: ({
    isOpen,
    runId,
    defaultModel,
    width,
  }: {
    isOpen: boolean;
    runId: string;
    defaultModel: string | null;
    width: number;
  }) =>
    createElement("aside", {
      "aria-label":         "Ask Fabro",
      "aria-hidden":        !isOpen,
      "data-run-id":        runId,
      "data-default-model": defaultModel,
      "data-width":         width,
    }),
}));

const mutationState = () => ({
  data:       null,
  error:      null,
  isMutating: false,
  reset:      mock(() => undefined),
  trigger:    mock(() => Promise.resolve(undefined)),
});

mock.module("../lib/mutations", () => ({
  useArchiveRun:           mutationState,
  useApproveRun:           mutationState,
  useCancelRun:            mutationState,
  useDenyRun:              mutationState,
  useInterruptRun:         mutationState,
  usePreviewRun:           mutationState,
  useRetryRun:             mutationState,
  useSteerRun:             mutationState,
  useSubmitInterviewAnswer: mutationState,
  useUpdateRunTitle:       mutationState,
  useUnarchiveRun:         mutationState,
}));

import {
  actionMenuSeparatorVisibility,
  focusSteerAfterMenuClose,
} from "./run-detail/actions";
import {
  handleLifecycleToastResult,
  lifecycleActionVisibility,
} from "./run-detail/lifecycle-toasts";

const { default: RunDetail } = await import("./run-detail");
mock.restore();
type LifecycleToastState = import("./run-detail/lifecycle-toasts").LifecycleToastState;
type RunDetailActionResult = import("./run-detail/lifecycle-toasts").RunDetailActionResult;

const h = createElement;

function makeRunSummary(
  status = "succeeded",
  diffSummary: any = null,
  pullRequest: any = null,
  title = "Run 1",
  askFabro: any = null,
) {
  const apiStatus =
    status === "succeeded"
      ? { kind: "succeeded", reason: "completed" }
      : status === "failed"
        ? { kind: "failed", reason: "workflow_error" }
        : status === "dead"
          ? { kind: "dead" }
          : status === "blocked"
            ? { kind: "blocked", blocked_reason: "human_input_required" }
            : { kind: status };
  const archived = status === "archived";
  return {
    id:               "run_1",
    goal:             "Run 1",
    title,
    workflow:         { slug: "default", name: "Default", graph_name: null, node_count: 0, edge_count: 0 },
    automation:       null,
    repository:       { name: "fabro", origin_url: null, provider: "unknown" },
    created_by:       null,
    origin:           { kind: "api" },
    labels:           {},
    lifecycle:        {
      status:          archived ? { kind: "succeeded", reason: "completed" } : apiStatus,
      approval:        null,
      pending_control: null,
      queue_position:  null,
      error:           null,
      archived,
      archived_at:     archived ? "2026-04-20T12:05:00Z" : null,
    },
    sandbox:          null,
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
    diff:             diffSummary,
    pull_request:     pullRequest,
    current_question: null,
    superseded_by:    null,
    retried_from:     null,
    ask_fabro:        askFabro,
    links:            { web: null },
  };
}

function makeQuestion() {
  return {
    id:              "q_1",
    text:            "Approve?",
    stage:           "review",
    question_type:   QuestionType.YES_NO,
    options:         [],
    allow_freeform:  false,
    timeout_seconds: null,
    context_display: null,
  };
}

function RunDetailWithParams() {
  const params = useParams();
  return h(RunDetail, { params: params as { id: string } });
}

async function renderRunDetailHarness({
  initialEntry,
  status = "succeeded",
  questions = [],
  diffSummary = null,
  pullRequest = null,
  title,
  askFabro = null,
}: {
  initialEntry: string;
  status?: string;
  questions?: any[];
  diffSummary?: any;
  pullRequest?: any;
  title?: string;
  askFabro?: any;
}) {
  currentRunSummary = makeRunSummary(status, diffSummary, pullRequest, title, askFabro);
  currentQuestions = questions;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

  const router = createMemoryRouter(
    [
      {
        path:    "/runs",
        element: h("div", { "data-route": "runs-index" }, "Runs"),
      },
      {
        path:    "/runs/:id",
        element: h(RunDetailWithParams),
        children: [
          {
            index:   true,
            element: h("div", { "data-child-route": "overview" }, "Overview"),
          },
          {
            path:    "files",
            handle:  { fullHeight: true },
            element: h("div", { "data-child-route": "files" }, "Files"),
          },
        ],
      },
    ],
    { initialEntries: [initialEntry] },
  );

  let renderer: TestRenderer.ReactTestRenderer | undefined;
  await act(async () => {
    renderer = TestRenderer.create(
      h(
        DemoModeProvider,
        { value: false },
        h(ToastProvider, null, h(RouterProvider, { router })),
      ),
    );
  });
  mountedRenderers.push(renderer!);
  return { renderer: renderer!, router };
}

async function renderRunDetail(
  options: Parameters<typeof renderRunDetailHarness>[0],
) {
  const { renderer } = await renderRunDetailHarness(options);
  return renderer;
}

function hasClasses(value: unknown, classes: string[]) {
  const tokens = String(value ?? "").split(/\s+/);
  return classes.every((className) => tokens.includes(className));
}

function tabCountBadges(renderer: TestRenderer.ReactTestRenderer) {
  return renderer.root.findAll(
    (node) =>
      node.type === "span" &&
      hasClasses(node.props.className, ["rounded-full", "tabular-nums"]),
  );
}

function askFabroButtons(renderer: TestRenderer.ReactTestRenderer) {
  return renderer.root.findAll(
    (node) => node.type === "button" && node.children.includes("Ask Fabro"),
  );
}

function textFromNode(
  node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>,
): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textFromNode).join(" ");
  return (node.children ?? []).map(textFromNode).join(" ");
}

function textFromTestNode(node: TestRenderer.ReactTestInstance): string {
  return node.children.map((child) => {
    if (typeof child === "string") return child;
    if (typeof child === "number") return String(child);
    return textFromTestNode(child);
  }).join("");
}

function findButtonByText(
  renderer: TestRenderer.ReactTestRenderer,
  text: string,
) {
  return renderer.root.findAll(
    (node) => node.type === "button" && textFromTestNode(node).includes(text),
  )[0];
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((promiseResolve, promiseReject) => {
    resolve = promiseResolve;
    reject = promiseReject;
  });
  return { promise, resolve, reject };
}

describe("lifecycleActionVisibility", () => {
  test("shows cancel for active cancellable states and hides it elsewhere", () => {
    expect(lifecycleActionVisibility("submitted").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("pending").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("runnable").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("starting").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("running").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("paused").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("blocked").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("succeeded").showPrimaryCancel).toBe(false);
    expect(lifecycleActionVisibility("failed").showPrimaryCancel).toBe(false);
    expect(lifecycleActionVisibility("dead").showPrimaryCancel).toBe(false);
    expect(lifecycleActionVisibility("archived").showPrimaryCancel).toBe(false);
  });

  test("shows archive and unarchive in the expected terminal states", () => {
    expect(lifecycleActionVisibility("succeeded").showArchive).toBe(true);
    expect(lifecycleActionVisibility("failed").showArchive).toBe(true);
    expect(lifecycleActionVisibility("dead").showArchive).toBe(true);
    expect(lifecycleActionVisibility("archived").showArchive).toBe(false);
    expect(lifecycleActionVisibility("archived").showUnarchive).toBe(true);
    expect(lifecycleActionVisibility("running").showUnarchive).toBe(false);
  });
});

describe("actionMenuSeparatorVisibility", () => {
  test("does not render adjacent dividers when destructive actions follow ops directly", () => {
    expect(
      actionMenuSeparatorVisibility({
        operations:   3,
        lifecycle:    0,
        destructive:  1,
      }),
    ).toEqual({
      afterOperations:   true,
      beforeDestructive: false,
    });
  });

  test("renders both dividers when lifecycle actions sit between ops and destructive actions", () => {
    expect(
      actionMenuSeparatorVisibility({
        operations:   3,
        lifecycle:    1,
        destructive:  1,
      }),
    ).toEqual({
      afterOperations:   true,
      beforeDestructive: true,
    });
  });
});

describe("handleLifecycleToastResult", () => {
  type PushedToast = { message: string };

  function makeToastApi() {
    const pushed: PushedToast[] = [];
    const dismissed: string[] = [];
    return {
      pushed,
      dismissed,
      api: {
        push: (toast: PushedToast) => {
          pushed.push(toast);
          return `toast-${pushed.length}`;
        },
        dismiss: (id: string) => {
          dismissed.push(id);
        },
      },
    };
  }

  const initialState: LifecycleToastState = {
    activeArchiveToastId: null,
    lastProcessed: {
      cancel:    null,
      approve:   null,
      deny:      null,
      archive:   null,
      unarchive: null,
      retry:     null,
    },
  };

  test("replaying the same cancel success result does not enqueue a duplicate toast", () => {
    const { pushed, dismissed, api } = makeToastApi();
    const result: RunDetailActionResult = {
      intent: "cancel",
      ok: true,
      run: makeRunSummary("failed"),
    };
    result.run.lifecycle.status = { kind: "failed", reason: "cancelled" };

    const firstState = handleLifecycleToastResult("cancel", result, initialState, api);

    expect(pushed).toEqual([{ message: "Run cancelled." }]);
    expect(firstState.lastProcessed.cancel).toBe(result);

    const replayedState = handleLifecycleToastResult("cancel", result, firstState, api);

    expect(pushed).toHaveLength(1);
    expect(dismissed).toEqual([]);
    expect(replayedState).toBe(firstState);
  });

  test("cancel for non-terminal state reports cancellation as requested", () => {
    const { pushed, api } = makeToastApi();
    const result: RunDetailActionResult = {
      intent: "cancel",
      ok: true,
      run: makeRunSummary("running"),
    };

    handleLifecycleToastResult("cancel", result, initialState, api);

    expect(pushed).toEqual([{ message: "Cancellation requested." }]);
  });

  test("replaying the same archive success result does not enqueue a duplicate toast", () => {
    const { pushed, dismissed, api } = makeToastApi();
    const result: RunDetailActionResult = {
      intent: "archive",
      ok: true,
      run: makeRunSummary("archived"),
    };

    const firstState = handleLifecycleToastResult("archive", result, initialState, api);

    expect(pushed).toEqual([{ message: "Run archived." }]);
    expect(firstState.activeArchiveToastId).toBe("toast-1");

    const replayedState = handleLifecycleToastResult("archive", result, firstState, api);

    expect(pushed).toHaveLength(1);
    expect(replayedState).toBe(firstState);
    expect(dismissed).toEqual([]);
  });

  test("successful unarchive dismisses the active archive toast before showing restore feedback", () => {
    const { pushed, dismissed, api } = makeToastApi();
    const result: RunDetailActionResult = {
      intent: "unarchive",
      ok: true,
      run: makeRunSummary("succeeded"),
    };
    const stateWithActiveToast: LifecycleToastState = {
      activeArchiveToastId: "toast-9",
      lastProcessed: {
        cancel:    null,
        approve:   null,
        deny:      null,
        archive:   null,
        unarchive: null,
        retry:     null,
      },
    };

    const nextState = handleLifecycleToastResult("unarchive", result, stateWithActiveToast, api);

    expect(dismissed).toEqual(["toast-9"]);
    expect(pushed).toEqual([{ message: "Run restored." }]);
    expect(nextState.activeArchiveToastId).toBeNull();

    const replayedState = handleLifecycleToastResult("unarchive", result, nextState, api);

    expect(dismissed).toEqual(["toast-9"]);
    expect(pushed).toEqual([{ message: "Run restored." }]);
    expect(replayedState).toBe(nextState);
  });
});

describe("RunDetail full-height child routes", () => {
  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    currentRunSummary = null;
    currentRunState = null;
    currentQuestions = [];
    deleteRunApiResult = null;
    deleteRunApiMock.mockClear();
    mutateRunListCachesMock.mockClear();
    swrMutateMock.mockClear();
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  test("uses a full-height flex wrapper for fullHeight child routes", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1/files",
    });

    const fullHeightRoot = renderer.root.findAll(
      (node) =>
        node.type === "div" &&
        hasClasses(node.props.className, ["h-full", "min-h-0", "flex", "flex-col"]),
    );
    expect(fullHeightRoot.length).toBeGreaterThan(0);

    const outletWrappers = renderer.root.findAll(
      (node) =>
        node.type === "div" &&
        hasClasses(node.props.className, ["pt-3", "min-h-0", "flex-1", "flex-col"]),
    );
    expect(outletWrappers).toHaveLength(1);
  });

  test("shows the Files Changed tab badge from run summary diff stats", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1/files",
      diffSummary:  {
        files_changed: 7,
        additions:     30,
        deletions:     11,
      },
    });

    const badges = tabCountBadges(renderer);
    expect(badges.map((badge) => badge.children.join(""))).toContain("7");
  });

  test("successful retry result navigates to the new run once", () => {
    const pushed: Array<{ message: string; tone?: string }> = [];
    const navigated: string[] = [];
    const result: RunDetailActionResult = {
      intent: "retry",
      ok:     true,
      run:    {
        ...makeRunSummary("runnable"),
        id:           "run_retry",
        retried_from: "run_1",
      },
    };
    const initialState: LifecycleToastState = {
      activeArchiveToastId: null,
      lastProcessed:        {
        cancel:    null,
        approve:   null,
        deny:      null,
        archive:   null,
        unarchive: null,
        retry:     null,
      },
    };

    const next = handleLifecycleToastResult(
      "retry",
      result,
      initialState,
      {
        push:    (toast) => {
          pushed.push(toast);
          return "toast-1";
        },
        dismiss: () => undefined,
      },
      (path) => navigated.push(path),
    );
    const replay = handleLifecycleToastResult(
      "retry",
      result,
      next,
      {
        push:    (toast) => {
          pushed.push(toast);
          return "toast-2";
        },
        dismiss: () => undefined,
      },
      (path) => navigated.push(path),
    );

    expect(next.lastProcessed.retry).toBe(result);
    expect(replay).toBe(next);
    expect(pushed).toEqual([{ message: "Retry started." }]);
    expect(navigated).toEqual(["/runs/run_retry"]);
  });

  test("hides the Sandbox tab for a planned sandbox without an instance", async () => {
    currentRunState = {
      sandbox: {
        kind: "planned",
        plan: { provider: "docker", image: null, snapshot: null },
      },
    };
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1",
    });

    const sandboxLinks = renderer.root.findAll(
      (node) =>
        node.type === "a" &&
        node.props.href === "/runs/run_1/sandbox",
    );
    expect(sandboxLinks).toHaveLength(0);
  });

  for (const kind of ["initializing", "ready", "failed"] as const) {
    test(`shows the Sandbox tab for ${kind} sandbox state`, async () => {
      currentRunState = {
        sandbox: {
          kind,
          plan: { provider: "docker", image: null, snapshot: null },
          instance: kind === "ready"
            ? {
              provider: "docker",
              image: null,
              snapshot: null,
              runtime: {
                id: "container-1",
                working_directory: "/workspace",
                repo_cloned: null,
                clone_origin_url: null,
                clone_branch: null,
              },
            }
            : undefined,
          failure: kind === "failed"
            ? {
              provider: "docker",
              error: "Docker daemon unavailable",
              causes: [],
              duration_ms: 42,
            }
            : undefined,
        },
      };
      const renderer = await renderRunDetail({
        initialEntry: "/runs/run_1",
      });

      const sandboxLinks = renderer.root.findAll(
        (node) =>
          node.type === "a" &&
          node.props.href === "/runs/run_1/sandbox" &&
          node.children.includes("Sandbox"),
      );
      expect(sandboxLinks).toHaveLength(1);
    });
  }

  test("shows the Sandbox tab for legacy sandbox state with runtime metadata", async () => {
    currentRunState = {
      sandbox: {
        provider: "docker",
        runtime: {
          id:                "container-1",
          working_directory: "/workspace",
        },
      },
    };
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1",
    });

    const sandboxLinks = renderer.root.findAll(
      (node) =>
        node.type === "a" &&
        node.props.href === "/runs/run_1/sandbox" &&
        node.children.includes("Sandbox"),
    );
    expect(sandboxLinks).toHaveLength(1);
  });

  test("hides the Sandbox tab when the run has no sandbox", async () => {
    currentRunState = {};
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1",
    });

    const sandboxLinks = renderer.root.findAll(
      (node) =>
        node.type === "a" &&
        node.props.href === "/runs/run_1/sandbox",
    );
    expect(sandboxLinks).toHaveLength(0);
  });

  test("defers steer bar focus until after the Actions menu item click settles", async () => {
    const focusCalls: string[] = [];

    focusSteerAfterMenuClose(() => focusCalls.push("focus"));

    expect(focusCalls).toEqual([]);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(focusCalls).toEqual(["focus"]);
  });

  test("hides the Files Changed tab badge when diff stats are absent", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1/files",
    });

    expect(tabCountBadges(renderer)).toHaveLength(0);
  });

  test("confirms deleting an archived run and navigates back to runs", async () => {
    const deletion = deferred<unknown>();
    deleteRunApiResult = deletion.promise;
    const { renderer, router } = await renderRunDetailHarness({
      initialEntry: "/runs/run_1",
      status:       "archived",
    });

    const actionsButton = findButtonByText(renderer, "Actions");
    expect(actionsButton).toBeDefined();

    await act(async () => {
      actionsButton!.props.onClick({
        currentTarget:    { parentElement: null },
        defaultPrevented: false,
        preventDefault:  () => undefined,
      });
    });

    const deleteButton = findButtonByText(renderer, "Delete");
    expect(deleteButton).toBeDefined();

    await act(async () => {
      deleteButton!.props.onClick();
    });

    const confirmButton = findButtonByText(renderer, "Delete run");
    expect(confirmButton).toBeDefined();

    await act(async () => {
      confirmButton!.props.onClick();
      await Promise.resolve();
    });

    const pendingButton = findButtonByText(renderer, "Deleting…");
    expect(pendingButton).toBeDefined();
    expect(pendingButton!.props.disabled).toBe(true);

    await act(async () => {
      deletion.resolve({});
      await deletion.promise;
      await Promise.resolve();
    });

    expect(deleteRunApiMock).toHaveBeenCalledTimes(1);
    expect(deleteRunApiMock.mock.calls[0]?.[0]).toBe("run_1");
    expect(mutateRunListCachesMock).toHaveBeenCalledTimes(1);
    expect(mutateRunListCachesMock.mock.calls[0]?.[0]).toBe(swrMutateMock);
    expect(textFromNode(renderer.toJSON())).toContain("Run deleted.");
    expect(router.state.location.pathname).toBe("/runs");
  });

  test("disables Ask Fabro trigger when the run reports an unavailable reason", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1",
      askFabro: {
        available:          false,
        unavailable_reason: AskFabroUnavailableReasonEnum.NO_SANDBOX,
        default_model:      null,
      },
    });

    const buttons = askFabroButtons(renderer);
    expect(buttons).toHaveLength(1);
    expect(buttons[0].props.disabled).toBe(true);
    expect(
      renderer.root.findAll(
        (node) => node.type === "aside" && node.props["aria-label"] === "Ask Fabro",
      ),
    ).toHaveLength(0);
  });

  test("mounts the Ask Fabro sidebar and opens it from the trigger when available", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1",
      askFabro: {
        available:          true,
        unavailable_reason: null,
        default_model:      "gpt-5",
      },
    });

    const buttons = askFabroButtons(renderer);
    expect(buttons).toHaveLength(1);
    expect(buttons[0].props.disabled).toBe(false);

    let sidebars = renderer.root.findAll(
      (node) => node.type === "aside" && node.props["aria-label"] === "Ask Fabro",
    );
    expect(sidebars).toHaveLength(1);
    expect(sidebars[0].props["aria-hidden"]).toBe(true);
    expect(sidebars[0].props["data-run-id"]).toBe("run_1");
    expect(sidebars[0].props["data-default-model"]).toBe("gpt-5");

    await act(async () => {
      buttons[0].props.onClick();
    });

    sidebars = renderer.root.findAll(
      (node) => node.type === "aside" && node.props["aria-label"] === "Ask Fabro",
    );
    expect(sidebars[0].props["aria-hidden"]).toBe(false);
  });

  test("shows a linked pull request pill in the run header", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1",
      pullRequest: {
        owner: "fabro-sh",
        repo: "fabro",
        number: 123,
        html_url: "https://github.com/fabro-sh/fabro/pull/123",
      },
    });

    const links = renderer.root.findAll(
      (node) =>
        node.type === "a" &&
        node.props.href === "https://github.com/fabro-sh/fabro/pull/123",
    );

    expect(links).toHaveLength(1);
    expect(links[0].props.target).toBe("_blank");
    const numberSpan = links[0].findByType("span");
    expect(
      numberSpan.children.filter((child) => typeof child !== "object").join(""),
    ).toBe("#123");
  });

  test("keeps blocked full-height children clear of the interview dock without an h-72 sibling", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1/files",
      status:       "blocked",
      questions:    [makeQuestion()],
    });

    const spacers = renderer.root.findAll(
      (node) => node.type === "div" && hasClasses(node.props.className, ["h-72"]),
    );
    expect(spacers).toHaveLength(0);

    const dock = renderer.root.findAll(
      (node) =>
        node.type === "section" &&
        node.props["aria-label"] === "Interview question",
    );
    expect(dock).toHaveLength(1);

    const clearanceOwners = renderer.root.findAll(
      (node) =>
        node.type === "div" &&
        node.props.style?.["--fabro-interview-dock-clearance"] === "18rem",
    );
    expect(clearanceOwners.length).toBeGreaterThan(0);
  });

  test("renders inline <code> in the run title heading for Markdown-formatted titles", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1",
      title:        "Move from `[server.integrations.github]` to `[run.integrations.github]`",
    });

    const headings = renderer.root.findAll(
      (node) =>
        node.type === "h2" &&
        hasClasses(node.props.className, ["text-xl", "font-semibold", "text-fg"]),
    );
    expect(headings).toHaveLength(1);

    const codes = headings[0]!.findAllByType("code");
    expect(codes).toHaveLength(2);
    expect(
      codes
        .map((code) =>
          code.children.filter((child) => typeof child === "string").join(""),
        ),
    ).toEqual([
      "[server.integrations.github]",
      "[run.integrations.github]",
    ]);
  });

  test("preserves document-flow layout for child routes without fullHeight", async () => {
    const renderer = await renderRunDetail({
      initialEntry: "/runs/run_1",
    });

    const fullHeightRoot = renderer.root.findAll(
      (node) =>
        node.type === "div" &&
        hasClasses(node.props.className, ["h-full", "min-h-0", "flex", "flex-col"]),
    );
    expect(fullHeightRoot).toHaveLength(0);

    const outletWrappers = renderer.root.findAll(
      (node) =>
        node.type === "div" &&
        hasClasses(node.props.className, [
          "pt-3",
          "pb-[var(--fabro-interview-dock-clearance)]",
        ]),
    );
    expect(outletWrappers).toHaveLength(1);
  });
});
