import { afterEach, describe, expect, mock, test } from "bun:test";
import { createElement } from "react";
import TestRenderer, { act } from "react-test-renderer";
import {
  createMemoryRouter,
  RouterProvider,
  useParams,
} from "react-router";
import { QuestionType } from "@qltysh/fabro-api-client";

import { ToastProvider } from "../components/toast";
import { DemoModeProvider } from "../lib/demo-mode";

let currentRunSummary: any = null;
let currentRunState: any = null;
let currentQuestions: any[] = [];
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

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

const {
  actionMenuSeparatorVisibility,
  default: RunDetail,
  focusSteerAfterMenuClose,
  handleLifecycleToastResult,
  lifecycleActionVisibility,
} = await import("./run-detail");
mock.restore();
type LifecycleToastState = import("./run-detail").LifecycleToastState;
type RunDetailActionResult = import("./run-detail").RunDetailActionResult;

const h = createElement;

function makeRunSummary(
  status = "succeeded",
  diffSummary: any = null,
  pullRequest: any = null,
  title = "Run 1",
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

async function renderRunDetail({
  initialEntry,
  status = "succeeded",
  questions = [],
  diffSummary = null,
  pullRequest = null,
  title,
}: {
  initialEntry: string;
  status?: string;
  questions?: any[];
  diffSummary?: any;
  pullRequest?: any;
  title?: string;
}) {
  currentRunSummary = makeRunSummary(status, diffSummary, pullRequest, title);
  currentQuestions = questions;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

  const router = createMemoryRouter(
    [
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
  return renderer!;
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
        hasLifecycle:  false,
        hasDestructive: true,
      }),
    ).toEqual({
      afterOperations:   true,
      beforeDestructive: false,
    });
  });

  test("renders both dividers when lifecycle actions sit between ops and destructive actions", () => {
    expect(
      actionMenuSeparatorVisibility({
        hasLifecycle:  true,
        hasDestructive: true,
      }),
    ).toEqual({
      afterOperations:   true,
      beforeDestructive: true,
    });
  });
});

describe("handleLifecycleToastResult", () => {
  type PushedToast = { message: string; action?: { label: string; onClick: () => void } };

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

  test("shows the Sandbox tab when the run has a sandbox", async () => {
    currentRunState = { sandbox: { provider: "docker", id: "container-1" } };
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

    const dock = renderer.root.findAllByProps({
      role:       "region",
      "aria-label": "Interview question",
    });
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
