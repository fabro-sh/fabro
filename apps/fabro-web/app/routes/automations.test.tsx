import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { createElement } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";
import useSWR, { SWRConfig, unstable_serialize } from "swr";
import type { Automation, AutomationListResponse, ReplaceAutomationRequest } from "@qltysh/fabro-api-client";

import { queryKeys } from "../lib/query-keys";
import { setupReactTestEnv, textContent } from "../lib/test-utils";

let initialAutomation: Automation;
let remoteAutomation: Automation;
let initialList: AutomationListResponse;
let remoteList: AutomationListResponse;
let renderer: TestRenderer.ReactTestRenderer;
let teardown: () => void;

class TestApiError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
  }
}

const replaceAutomationMock = mock((id: string, revision: string, request: ReplaceAutomationRequest) =>
  Promise.resolve({ data: { ...initialAutomation, ...request, id, revision: `${revision}-saved` } as Automation }),
);
const createRunMock = mock((_id: string) => Promise.resolve({ data: { id: "run-1" } }));
const listMock = mock(() => Promise.resolve(remoteList));
const detailMock = mock(() => Promise.resolve(remoteAutomation));
const toastMock = mock((_toast: { message: string; tone?: string }) => "toast-1");

mock.module("../lib/queries", () => ({
  useAutomations: () => useSWR(queryKeys.automations.list(), listMock),
}));
mock.module("../lib/api-client", () => ({
  ApiError: TestApiError,
  apiData: async <T,>(call: () => Promise<{ data: T }>): Promise<T> => (await call()).data,
  automationsApi: {
    replaceAutomation: replaceAutomationMock,
    createAutomationRun: createRunMock,
  },
}));
mock.module("../components/toast", () => ({ useToast: () => ({ push: toastMock }) }));

// Headless UI needs a DOM; these wrappers preserve the route's controls in the renderer.
mock.module("@headlessui/react", () => ({
  Menu: ({ children }: any) => createElement("div", null, children),
  MenuButton: ({ children, ...props }: any) => createElement("button", props, children),
  MenuItems: ({ children }: any) => createElement("div", null, children),
  MenuItem: ({ children }: any) => typeof children === "function" ? children({ focus: false }) : children,
  Dialog: () => null,
  DialogPanel: ({ children }: any) => children,
  DialogTitle: ({ children }: any) => children,
}));

const { default: Automations } = await import("./automations");
mock.restore();

function automation(overrides: Partial<Automation> = {}): Automation {
  return {
    id: "nightly",
    revision: "revision-1",
    name: "Nightly checks",
    description: "Check the release branch",
    environment_id: "docker-ci",
    last_error: null,
    target: { kind: "git", repo: "example/app", branch: "release", tag: null, sha: null },
    workflow: "check",
    workflow_source: { kind: "git", repo: "example/workflows", branch: "main", tag: "v1", sha: null },
    triggers: [
      { type: "api", id: "manual", enabled: true },
      { type: "schedule", id: "daily", enabled: true, expression: "0 9 * * *" },
    ],
    ...overrides,
  };
}

function DetailProbe() {
  const { data } = useSWR(queryKeys.automations.detail(initialAutomation.id), detailMock);
  return <output aria-label="Detail revision">{data?.revision}</output>;
}

function renderAutomations() {
  act(() => {
    renderer = TestRenderer.create(
      <SWRConfig value={{
        provider: () => new Map(),
        fallback: {
          [unstable_serialize(queryKeys.automations.list())]: initialList,
          [unstable_serialize(queryKeys.automations.detail(initialAutomation.id))]: initialAutomation,
        },
        revalidateOnMount: false,
        revalidateOnFocus: false,
        revalidateOnReconnect: false,
        shouldRetryOnError: false,
      }}>
        <MemoryRouter initialEntries={["/automations"]}>
          <Automations />
          <DetailProbe />
        </MemoryRouter>
      </SWRConfig>,
    );
  });
}

function button(label: string) {
  return renderer.root.findAllByType("button").find((node) => node.props["aria-label"] === label)!;
}

function pauseLabel(id = "daily", name = initialAutomation.name) {
  return `Pause schedule ${id} for ${name}`;
}

function resumeLabel(id = "daily", name = initialAutomation.name) {
  return `Resume schedule ${id} for ${name}`;
}

async function click(label: string) {
  await act(async () => { await button(label).props.onClick(); });
}

describe("automation schedule controls", () => {
  beforeEach(() => {
    teardown = setupReactTestEnv();
    initialAutomation = automation();
    remoteAutomation = initialAutomation;
    initialList = { data: [initialAutomation], meta: { total: 1 } };
    remoteList = initialList;
    replaceAutomationMock.mockClear();
    replaceAutomationMock.mockImplementation((id, revision, request) =>
      Promise.resolve({ data: { ...initialAutomation, ...request, id, revision: `${revision}-saved` } as Automation }),
    );
    createRunMock.mockClear();
    listMock.mockClear();
    detailMock.mockClear();
    toastMock.mockClear();
  });

  afterEach(() => {
    act(() => renderer?.unmount());
    teardown();
  });

  test("pauses and resumes with confirmed state and revision, preserving the automation", async () => {
    renderAutomations();
    await click(pauseLabel());

    expect(replaceAutomationMock.mock.calls[0]).toEqual([
      "nightly", "revision-1", {
        name: initialAutomation.name,
        description: initialAutomation.description,
        environment_id: initialAutomation.environment_id,
        target: initialAutomation.target,
        workflow: initialAutomation.workflow,
        workflow_source: initialAutomation.workflow_source,
        triggers: [
          initialAutomation.triggers[0],
          { ...initialAutomation.triggers[1], enabled: false },
        ],
      },
    ]);
    expect(textContent(renderer.root)).toContain("0 9 * * *· Paused");
    expect(button(resumeLabel())).toBeDefined();
    expect(renderer.root.findByType("output").children).toEqual(["revision-1-saved"]);

    await click(resumeLabel());
    expect(replaceAutomationMock.mock.calls[1][1]).toBe("revision-1-saved");
    expect(replaceAutomationMock.mock.calls[1][2].triggers[1].enabled).toBe(true);
    expect(button(pauseLabel())).toBeDefined();
    expect(textContent(renderer.root)).not.toContain("Paused");
    expect(renderer.root.findByType("output").children).toEqual(["revision-1-saved-saved"]);
    expect(toastMock.mock.calls.map(([toast]) => toast.tone)).toEqual([undefined, undefined]);
  });

  test("shows API-disabled schedules as paused on initial load", () => {
    initialAutomation.triggers[1].enabled = false;
    renderAutomations();
    expect(button(resumeLabel())).toBeDefined();
    expect(button(pauseLabel())).toBeUndefined();
    expect(textContent(renderer.root)).toContain("Paused");
    expect(replaceAutomationMock).not.toHaveBeenCalled();
  });

  test("controls every schedule without changing sibling triggers or their state", async () => {
    initialAutomation.triggers.push(
      { type: "schedule", id: "weekly", enabled: true, expression: "0 10 * * 1" },
      { type: "schedule", id: "monthly", enabled: false, expression: "0 11 1 * *" },
    );
    renderAutomations();
    expect(button(pauseLabel("weekly"))).toBeDefined();
    expect(button(resumeLabel("monthly"))).toBeDefined();

    await click(pauseLabel("weekly"));
    expect(replaceAutomationMock.mock.calls[0][2].triggers).toEqual([
      ...initialAutomation.triggers.slice(0, 2),
      { ...initialAutomation.triggers[2], enabled: false },
      initialAutomation.triggers[3],
    ]);
    expect(button(pauseLabel())).toBeDefined();
    expect(button(resumeLabel("weekly"))).toBeDefined();
    expect(button(resumeLabel("monthly"))).toBeDefined();
  });

  test("prevents duplicate updates and keeps saved state unchanged while the request is pending", async () => {
    let finish!: (result: { data: Automation }) => void;
    replaceAutomationMock.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    initialAutomation.triggers.push({ type: "schedule", id: "weekly", enabled: true, expression: "0 10 * * 1" });
    renderAutomations();
    const onClick = button(pauseLabel()).props.onClick;
    act(() => { onClick(); onClick(); });

    expect(replaceAutomationMock).toHaveBeenCalledTimes(1);
    expect(button(pauseLabel()).props.disabled).toBe(true);
    expect(button(pauseLabel()).props["aria-busy"]).toBe(true);
    expect(button(pauseLabel("weekly")).props.disabled).toBe(true);
    expect(textContent(button(pauseLabel()))).toBe("Saving…");
    expect(textContent(renderer.root)).not.toContain("Paused");
    expect(toastMock).not.toHaveBeenCalled();

    await act(async () => {
      finish({ data: automation({ revision: "revision-2", triggers: [
        initialAutomation.triggers[0], { ...initialAutomation.triggers[1], enabled: false },
      ] }) });
    });
    expect(button(resumeLabel()).props.disabled).toBe(false);
  });

  test("other automations remain usable and concurrent saves preserve both rows", async () => {
    const other = automation({ id: "constructor", name: "Weekly checks" });
    initialList = { data: [initialAutomation, other], meta: { total: 2 } };
    const finish = new Map<string, () => void>();
    replaceAutomationMock.mockImplementation((id, revision, request) => new Promise((resolve) => {
      finish.set(id, () => resolve({ data: { ...initialAutomation, ...request, id, revision: `${revision}-saved` } as Automation }));
    }));
    renderAutomations();
    act(() => { button(pauseLabel()).props.onClick(); });
    expect(button(pauseLabel("daily", other.name)).props.disabled).toBe(false);
    act(() => { button(pauseLabel("daily", other.name)).props.onClick(); });
    expect(replaceAutomationMock).toHaveBeenCalledTimes(2);

    await act(async () => { finish.get(other.id)!(); });
    await act(async () => { finish.get(initialAutomation.id)!(); });
    expect(button(resumeLabel())).toBeDefined();
    expect(button(resumeLabel("daily", other.name))).toBeDefined();
  });

  test("failed saves show an error, preserve state, and allow a retry", async () => {
    replaceAutomationMock.mockRejectedValueOnce(new Error("offline"));
    renderAutomations();
    await click(pauseLabel());
    expect(button(pauseLabel()).props.disabled).toBe(false);
    expect(textContent(renderer.root)).not.toContain("Paused");
    expect(renderer.root.findByType("output").children).toEqual(["revision-1"]);
    expect(toastMock.mock.calls[0][0]).toEqual({
      tone: "error", message: "Couldn't pause the schedule. Please try again.",
    });
    await click(pauseLabel());
    expect(button(resumeLabel())).toBeDefined();
  });

  test("API validation failures are shown without claiming the schedule paused", async () => {
    replaceAutomationMock.mockRejectedValueOnce(new TestApiError(422, "automation environment not found: docker-ci"));
    renderAutomations();
    await click(pauseLabel());
    expect(toastMock.mock.calls[0][0]).toEqual({
      tone: "error", message: "automation environment not found: docker-ci",
    });
    expect(button(pauseLabel())).toBeDefined();
    expect(button(resumeLabel())).toBeUndefined();
  });

  test("conflicts reload list and detail without retrying or overwriting newer settings", async () => {
    replaceAutomationMock.mockRejectedValueOnce(new TestApiError(409, "automation revision is stale"));
    remoteAutomation = automation({ name: "Updated by another operator", revision: "revision-2" });
    remoteList = { ...initialList, data: [remoteAutomation] };
    renderAutomations();
    await click(pauseLabel());

    expect(replaceAutomationMock).toHaveBeenCalledTimes(1);
    expect(listMock).toHaveBeenCalledTimes(1);
    expect(detailMock).toHaveBeenCalledTimes(1);
    expect(toastMock.mock.calls[0][0].tone).toBe("error");
    expect(toastMock.mock.calls[0][0].message).toContain("changed since it was loaded");
    expect(renderer.root.findByType("output").children).toEqual(["revision-2"]);

    await click(pauseLabel("daily", remoteAutomation.name));
    expect(replaceAutomationMock.mock.calls[1][1]).toBe("revision-2");
    expect(replaceAutomationMock.mock.calls[1][2].name).toBe(remoteAutomation.name);
  });

  test("paused schedules stay in the Scheduled filter", async () => {
    initialAutomation.triggers[1].enabled = false;
    renderAutomations();
    await act(async () => {
      renderer.root.findAllByType("button").find((node) => textContent(node) === "Scheduled")!.props.onClick();
    });
    expect(button(resumeLabel())).toBeDefined();
    await act(async () => {
      renderer.root.findAllByType("button").find((node) => textContent(node) === "Manual")!.props.onClick();
    });
    expect(button(resumeLabel())).toBeUndefined();
  });

  test("automations without schedules keep the manual run action", async () => {
    initialAutomation.triggers = [initialAutomation.triggers[0]];
    renderAutomations();
    await click("Run automation");
    expect(createRunMock).toHaveBeenCalledWith("nightly");
    expect(replaceAutomationMock).not.toHaveBeenCalled();
  });
});
