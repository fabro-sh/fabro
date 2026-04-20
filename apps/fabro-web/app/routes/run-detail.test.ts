import { afterEach, describe, expect, test } from "bun:test";

import { action, lifecycleActionVisibility, loader } from "./run-detail";

type StubFetchEntry = {
  status: number;
  body?: unknown;
};

function stubFetchSequence(entries: StubFetchEntry[]) {
  const originalFetch = globalThis.fetch;
  let index = 0;

  globalThis.fetch = ((input: RequestInfo | URL) => {
    const next = entries[index++];
    if (!next) {
      throw new Error(`unexpected fetch for ${String(input)}`);
    }
    return Promise.resolve(
      new Response(next.body == null ? "" : JSON.stringify(next.body), {
        status: next.status,
        headers: { "Content-Type": "application/json" },
      }),
    );
  }) as typeof fetch;

  return () => {
    globalThis.fetch = originalFetch;
  };
}

function buildActionRequest(data: Record<string, string>) {
  const formData = new FormData();
  for (const [key, value] of Object.entries(data)) {
    formData.set(key, value);
  }
  return new Request("http://fabro.test/runs/run-1", {
    method: "POST",
    body: formData,
  });
}

describe("run-detail loader", () => {
  let restoreFetch: (() => void) | undefined;

  afterEach(() => {
    restoreFetch?.();
    restoreFetch = undefined;
    delete (globalThis as { window?: unknown }).window;
  });

  test("loads the first blocked question when the run is blocked", async () => {
    restoreFetch = stubFetchSequence([
      {
        status: 200,
        body: {
          run_id: "run-1",
          title: "Blocked run",
          repository: { name: "repo" },
          status: "blocked",
          workflow_name: "review",
        },
      },
      {
        status: 200,
        body: {
          data: [{ id: "q-1", text: "Ship this change?", stage: "review", question_type: "single_select", options: [], allow_freeform: false }],
          meta: { has_more: false },
        },
      },
    ]);

    const result = await loader({
      request: new Request("http://fabro.test/runs/run-1"),
      params: { id: "run-1" },
    });

    expect(result.blockedQuestionText).toBe("Ship this change?");
    expect(result.run?.lifecycleStatus).toBe("blocked");
  });

  test("falls back to null blockedQuestionText when no question is available", async () => {
    restoreFetch = stubFetchSequence([
      {
        status: 200,
        body: {
          run_id: "run-1",
          title: "Blocked run",
          repository: { name: "repo" },
          status: "blocked",
          workflow_name: "review",
        },
      },
      {
        status: 200,
        body: {
          data: [],
          meta: { has_more: false },
        },
      },
    ]);

    const result = await loader({
      request: new Request("http://fabro.test/runs/run-1"),
      params: { id: "run-1" },
    });

    expect(result.blockedQuestionText).toBeNull();
  });
});

describe("run-detail action", () => {
  let restoreFetch: (() => void) | undefined;

  afterEach(() => {
    restoreFetch?.();
    restoreFetch = undefined;
    delete (globalThis as { window?: unknown }).window;
  });

  test("preview still dispatches through intent=preview", async () => {
    restoreFetch = stubFetchSequence([
      {
        status: 200,
        body: { url: "https://preview.example.com" },
      },
    ]);

    const result = await action({
      params: { id: "run-1" },
      request: buildActionRequest({
        intent: "preview",
        port: "3000",
        expires_in_secs: "3600",
      }),
    });

    expect(result).toEqual({
      intent: "preview",
      url: "https://preview.example.com",
    });
  });

  test("cancel dispatches through the lifecycle helper path", async () => {
    restoreFetch = stubFetchSequence([
      {
        status: 200,
        body: {
          id: "run-1",
          status: "failed",
          status_reason: "cancelled",
          created_at: "2026-04-20T12:00:00Z",
        },
      },
    ]);

    const result = await action({
      params: { id: "run-1" },
      request: buildActionRequest({ intent: "cancel" }),
    });

    expect(result).toEqual({
      intent: "cancel",
      ok: true,
      run: {
        id: "run-1",
        status: "failed",
        status_reason: "cancelled",
        created_at: "2026-04-20T12:00:00Z",
      },
    });
  });
});

describe("lifecycleActionVisibility", () => {
  test("shows cancel for active cancellable states and hides it elsewhere", () => {
    expect(lifecycleActionVisibility("submitted").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("queued").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("starting").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("running").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("paused").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("blocked").showPrimaryCancel).toBe(false);
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
    expect(lifecycleActionVisibility("blocked").showBlockedNotice).toBe(true);
  });
});
