import { describe, expect, test } from "bun:test";

import { createInitialDraft } from "../state/draft";
import { renderFabro } from "../files/render-fabro";
import { createPlaygroundAdapter } from "./runtime";

type AdapterRunInput = Parameters<
  ReturnType<typeof createPlaygroundAdapter>["run"]
>[0];

function runInput(text: string): AdapterRunInput {
  return {
    messages: [
      {
        role: "user",
        content: [{ type: "text", text }],
      },
    ],
    abortSignal: new AbortController().signal,
  } as unknown as AdapterRunInput;
}

async function drain(iter: AsyncGenerator<unknown>): Promise<void> {
  // eslint-disable-next-line no-empty
  for await (const _ of iter) {
  }
}

describe("createPlaygroundAdapter request body", () => {
  test("posts the rendered workflow.fabro under workflow_fabro", async () => {
    const draft = createInitialDraft();
    let captured: { url: string; body: unknown } | null = null;

    const adapter = createPlaygroundAdapter({
      chatEndpoint: "/api/v1/playground/chat",
      getWorkflow: () => draft,
      dispatch: () => {},
      fetchImpl: (async (url: string | URL | Request, init?: RequestInit) => {
        captured = {
          url: String(url),
          body: JSON.parse(String(init?.body)),
        };
        return new Response("", {
          status: 200,
          headers: { "Content-Type": "text/event-stream" },
        });
      }) as typeof fetch,
    });

    await drain(adapter.run(runInput("build me a workflow")));

    expect(captured).not.toBeNull();
    const { url, body } = captured! as {
      url: string;
      body: {
        workflow_fabro: string;
        workflow?: unknown;
        messages: unknown[];
      };
    };
    expect(url).toBe("/api/v1/playground/chat");
    expect(body.workflow_fabro).toBe(renderFabro(draft));
    expect(body.workflow_fabro).toContain("digraph Untitled");
    // The structured draft no longer rides in the request.
    expect(body.workflow).toBeUndefined();
    expect(body.messages).toHaveLength(1);
  });
});
