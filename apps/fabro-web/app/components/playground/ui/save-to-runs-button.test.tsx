import { afterEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import SaveToRunsButton from "./save-to-runs-button";
import { createInitialDraft, type WorkflowDraft } from "../state/draft";

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

type CapturedRequest = { url: string; method?: string; body: unknown };

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
    const req: CapturedRequest = {
      url:    String(url),
      method: init?.method,
      body:   init?.body ? JSON.parse(String(init.body)) : undefined,
    };
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

function findButton(tree: TestRenderer.ReactTestRenderer) {
  return tree.root.findByProps({ "aria-label": "Save to Runs" });
}

async function clickAndSettle(
  el: TestRenderer.ReactTestInstance,
): Promise<void> {
  await act(async () => {
    el.props.onClick();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("SaveToRunsButton", () => {
  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  test("POSTs the manifest to /api/v1/runs and links to the new run without starting it", async () => {
    const stub = stubFetch(() => ({
      ok:     true,
      status: 201,
      body:   { id: "run-9" },
    }));

    const tree = render(<SaveToRunsButton draft={withPlan()} />);
    await clickAndSettle(findButton(tree));

    // Exactly one request: create. No /start call (create-only).
    expect(stub.requests).toHaveLength(1);
    expect(stub.requests[0]!.url).toBe("/api/v1/runs");
    expect(stub.requests[0]!.method?.toUpperCase()).toBe("POST");
    expect(stub.requests.some((r) => r.url.includes("/start"))).toBe(false);

    const manifest = stub.requests[0]!.body as {
      version: number;
      target: { identifier: string };
    };
    expect(manifest.version).toBe(1);
    expect(manifest.target.identifier).toBe("release_notes");

    // Success surfaces a link into the runs list for the created run.
    expect(tree.root.findByProps({ href: "/runs/run-9" })).toBeDefined();
  });

  test("is disabled in the welcome state", () => {
    const tree = render(<SaveToRunsButton draft={createInitialDraft()} />);
    expect(findButton(tree).props.disabled).toBe(true);
  });

  test("surfaces the server error detail and creates no run link on failure", async () => {
    const stub = stubFetch(() => ({
      ok:         false,
      status:     400,
      statusText: "Bad Request",
      body:       { errors: [{ status: "400", title: "Bad Request", detail: "Validation failed" }] },
    }));

    const tree = render(<SaveToRunsButton draft={withPlan()} />);
    await clickAndSettle(findButton(tree));

    expect(stub.requests).toHaveLength(1);
    const alert = tree.root.findByProps({ role: "alert" });
    expect(alert.props.title).toContain("Validation failed");
    expect(tree.root.findAllByProps({ href: "/runs/run-9" })).toHaveLength(0);
  });
});
