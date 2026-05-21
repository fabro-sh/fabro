import { afterEach, describe, expect, mock, test } from "bun:test";

import { ApiError } from "./api-client";
import {
  attachSessionEvents,
  streamSessionTurn,
  type SessionStreamEvent,
} from "./session-stream";

const encoder = new TextEncoder();

afterEach(() => {
  mock.restore();
});

function streamResponse(chunks: string[], status = 200, headers: HeadersInit = {}) {
  return new Response(
    new ReadableStream({
      start(controller) {
        for (const chunk of chunks) {
          controller.enqueue(encoder.encode(chunk));
        }
        controller.close();
      },
    }),
    {
      status,
      headers: {
        "content-type": "text/event-stream",
        ...headers,
      },
    },
  );
}

describe("session stream helpers", () => {
  test("posts a turn and parses chunked SSE event envelopes", async () => {
    const events: SessionStreamEvent[] = [];
    const fetchMock = mock(() =>
      Promise.resolve(
        streamResponse(
          [
            "id: 3\nevent: run.session.turn.started\n",
            'data: {"seq":3,"event":{"event":"run.session.turn.started","properties":{"turn_id":"turn_1"}}}\n\n',
          ],
          200,
          { "x-fabro-turn-id": "turn_1" },
        ),
      ),
    );

    const result = await streamSessionTurn({
      sessionId: "ses_1",
      input: "Summarize",
      turnId: "turn_1",
      fetchImpl: fetchMock,
      onEvent: (event) => events.push(event),
    });

    expect(result.turnId).toBe("turn_1");
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/sessions/ses_1/turns");
    expect(JSON.parse(fetchMock.mock.calls[0]?.[1]?.body as string)).toEqual({
      input: "Summarize",
      turn_id: "turn_1",
    });
    expect(events).toHaveLength(1);
    expect(events[0]?.seq).toBe(3);
    expect(events[0]?.event.event).toBe("run.session.turn.started");
  });

  test("attaches to session events from a run sequence", async () => {
    const events: SessionStreamEvent[] = [];
    const fetchMock = mock(() =>
      Promise.resolve(
        streamResponse([
          'data: {"seq":7,"event":{"event":"run.session.assistant_message","properties":{}}}\n\n',
        ]),
      ),
    );

    await attachSessionEvents({
      sessionId: "ses_1",
      sinceSeq: 7,
      fetchImpl: fetchMock,
      onEvent: (event) => events.push(event),
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/sessions/ses_1/attach?since_seq=7",
    );
    expect(events[0]?.seq).toBe(7);
  });

  test("parses CRLF-delimited SSE frames", async () => {
    const events: SessionStreamEvent[] = [];
    const fetchMock = mock(() =>
      Promise.resolve(
        streamResponse([
          'data: {"seq":8,"event":{"event":"run.session.assistant_message","properties":{}}}\r\n\r\n',
        ]),
      ),
    );

    await attachSessionEvents({
      sessionId: "ses_1",
      fetchImpl: fetchMock,
      onEvent: (event) => events.push(event),
    });

    expect(events[0]?.seq).toBe(8);
  });

  test("converts non-2xx responses to ApiError", async () => {
    const fetchMock = mock(() =>
      Promise.resolve(
        new Response(
          JSON.stringify({
            errors: [{
              status: "409",
              title: "Conflict",
              detail: "Session already has an active turn.",
              code: "session_active_turn",
            }],
          }),
          {
            status: 409,
            headers: { "x-request-id": "req_1" },
          },
        ),
      ),
    );

    await expect(
      streamSessionTurn({
        sessionId: "ses_1",
        input: "Summarize",
        fetchImpl: fetchMock,
        onEvent: () => {},
      }),
    ).rejects.toMatchObject({
      status: 409,
      requestId: "req_1",
      message: "Session already has an active turn.",
    } satisfies Partial<ApiError>);
  });
});
