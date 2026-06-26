import { afterEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import RunForRealModal from "./run-for-real-modal";
import type { WorkflowDraft } from "../state/draft";

function withPlan(): WorkflowDraft {
  return {
    name:  "release_notes",
    goal:  "Generate release notes.",
    nodes: [
      { id: "start", label: "Start", shape: "mdiamond" },
      { id: "exit", label: "Exit", shape: "msquare" },
      { id: "plan", label: "Plan", shape: "box", prompt: "Plan it." },
    ],
    edges: [
      { from: "start", to: "plan" },
      { from: "plan", to: "exit" },
    ],
  };
}

function render(node: React.ReactNode): TestRenderer.ReactTestRenderer {
  let tree: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    tree = TestRenderer.create(node as TestRenderer.ReactTestRendererJSON);
  });
  return tree!;
}

type CapturedRequest = { url: string; method?: string };

function stubFetch(
  responder: (req: CapturedRequest) => {
    ok: boolean;
    status: number;
    statusText?: string;
    body?: unknown;
  },
): { requests: CapturedRequest[] } {
  const requests: CapturedRequest[] = [];
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    const req: CapturedRequest = { url: String(url), method: init?.method };
    requests.push(req);
    const res = responder(req);
    const payload = res.body ?? null;
    return {
      ok:         res.ok,
      status:     res.status,
      statusText: res.statusText ?? "",
      json:       async () => payload,
      clone:      () => ({ json: async () => payload }),
    } as unknown as Response;
  }) as typeof fetch;
  return { requests };
}

const originalFetch = globalThis.fetch;

/** Install a minimal `window` whose `location.assign` records the redirect. */
function stubWindowLocation(): { assigned: string[] } {
  const assigned: string[] = [];
  const stub = { location: { assign: (url: string) => void assigned.push(url) } };
  Object.defineProperty(globalThis, "window", {
    value: stub, writable: true, configurable: true,
  });
  return { assigned };
}

function launchButton(tree: TestRenderer.ReactTestRenderer) {
  return tree.root
    .findAll((n) => n.type === "button" && n.props.children === "Run in sandbox")[0]!;
}

async function clickAndSettle(el: TestRenderer.ReactTestInstance): Promise<void> {
  await act(async () => {
    el.props.onClick();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("RunForRealModal", () => {
  afterEach(() => {
    globalThis.fetch = originalFetch;
    delete (globalThis as { window?: unknown }).window;
  });

  test("creates the run, starts it, then redirects to its run page", async () => {
    const { assigned } = stubWindowLocation();
    const stub = stubFetch((req) =>
      req.url.endsWith("/start")
        ? { ok: true, status: 200 }
        : { ok: true, status: 201, body: { id: "run-7" } },
    );

    const tree = render(<RunForRealModal draft={withPlan()} onClose={() => {}} />);
    await clickAndSettle(launchButton(tree));

    // create first, then start — both POST, in order.
    expect(stub.requests.map((r) => `${r.method?.toUpperCase()} ${r.url}`)).toEqual([
      "POST /api/v1/runs",
      "POST /api/v1/runs/run-7/start",
    ]);
    // Only redirects once the run has actually been started.
    expect(assigned).toEqual(["/runs/run-7"]);
  });

  test("surfaces a start failure and does not redirect", async () => {
    const { assigned } = stubWindowLocation();
    stubFetch((req) =>
      req.url.endsWith("/start")
        ? {
            ok:         false,
            status:     409,
            statusText: "Conflict",
            body:       { errors: [{ status: "409", title: "Conflict", detail: "Run is not startable" }] },
          }
        : { ok: true, status: 201, body: { id: "run-7" } },
    );

    const tree = render(<RunForRealModal draft={withPlan()} onClose={() => {}} />);
    await clickAndSettle(launchButton(tree));

    expect(tree.root.findByProps({ className: "break-words" }).props.children).toContain(
      "Run is not startable",
    );
    expect(assigned).toHaveLength(0);
  });
});
