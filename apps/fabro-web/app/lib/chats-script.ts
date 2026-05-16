import type { ChatMessage } from "./chats-types";

/**
 * Scripted assistant replies cycled through per chat. Generic content,
 * intentionally not Fabro-specific. Each entry is a single assistant
 * ChatMessage; tool calls and their results are siblings in the content
 * array so the renderer can pair them.
 */
export const SCRIPTED_REPLIES: ChatMessage[] = [
  {
    role: "assistant",
    content: [
      {
        kind: "text",
        data: {
          text:
            "Hi! I'm a scripted prototype reply. A few things I can show off:\n\n" +
            "- Markdown rendering (lists, **bold**, *italics*, `code`)\n" +
            "- Streaming text appearing incrementally\n" +
            "- Tool calls with arguments and results\n" +
            "- Multi-paragraph responses with code blocks\n\n" +
            "Send another message to see the next response in the bank.",
        },
      },
    ],
  },
  {
    role: "assistant",
    content: [
      {
        kind: "text",
        data: {
          text:
            "Here's a TypeScript snippet that debounces a function:\n\n" +
            "```ts\n" +
            "export function debounce<T extends (...args: any[]) => void>(\n" +
            "  fn: T,\n" +
            "  ms: number,\n" +
            "): (...args: Parameters<T>) => void {\n" +
            "  let handle: ReturnType<typeof setTimeout> | undefined;\n" +
            "  return (...args) => {\n" +
            "    if (handle) clearTimeout(handle);\n" +
            "    handle = setTimeout(() => fn(...args), ms);\n" +
            "  };\n" +
            "}\n" +
            "```\n\n" +
            "The trailing-edge variant is the most common; a leading-edge variant fires immediately then suppresses subsequent calls.",
        },
      },
    ],
  },
  {
    role: "assistant",
    content: [
      {
        kind: "text",
        data: {
          text: "Let me search for that real quick.",
        },
      },
      {
        kind: "tool_call",
        data: {
          tool_call_id: "call_search_1",
          name: "search_web",
          arguments: {
            query: "current best practices for rate limiting an HTTP API",
            max_results: 5,
          },
        },
      },
      {
        kind: "tool_result",
        data: {
          tool_call_id: "call_search_1",
          content: {
            results: [
              {
                title: "Token bucket vs leaky bucket",
                url: "https://example.com/rate-limit-algorithms",
                snippet:
                  "Token bucket allows bursts, leaky bucket smooths traffic.",
              },
              {
                title: "Distributed rate limiting with Redis",
                url: "https://example.com/redis-rate-limit",
                snippet:
                  "INCR + EXPIRE is the simplest fixed-window approach.",
              },
            ],
          },
        },
      },
      {
        kind: "text",
        data: {
          text:
            "\n\nTwo solid starting points. For most APIs, a Redis-backed sliding window keyed by API key gives you per-tenant fairness without a lot of moving parts. For burst tolerance, a token-bucket per route is a nice layer on top.",
        },
      },
    ],
  },
  {
    role: "assistant",
    content: [
      {
        kind: "text",
        data: {
          text:
            "## The 4-fold path of refactoring a hook\n\n" +
            "When a React hook starts feeling tangled, work the corners in order:\n\n" +
            "### 1. Extract pure computation\n" +
            "Anything that is a function of inputs (no side effects, no state) leaves the hook entirely.\n\n" +
            "### 2. Collapse derived state into `useMemo`\n" +
            "State that is computable from other state shouldn't be its own state.\n\n" +
            "### 3. Split orthogonal concerns into sibling hooks\n" +
            "If two effects don't share dependencies, they don't belong in the same hook.\n\n" +
            "### 4. Promote to a reducer\n" +
            "Once there are 3+ related `useState` calls coordinating updates, `useReducer` makes the state machine explicit.\n\n" +
            "> The honest test: can you write a one-sentence description of what the hook is responsible for? If not, it's doing too much.",
        },
      },
    ],
  },
  {
    role: "assistant",
    content: [
      {
        kind: "text",
        data: {
          text: "I'll compute that for you.",
        },
      },
      {
        kind: "tool_call",
        data: {
          tool_call_id: "call_calc_1",
          name: "run_calculation",
          arguments: {
            expression: "compound_interest(principal=10000, rate=0.05, years=10)",
          },
        },
      },
      {
        kind: "tool_result",
        data: {
          tool_call_id: "call_calc_1",
          content: {
            value: 16288.95,
            currency: "USD",
            note: "Annual compounding; rounded to cents.",
          },
        },
      },
      {
        kind: "text",
        data: {
          text:
            "\n\n**$16,288.95** after 10 years. Bumping the rate to 7% would put you at roughly $19,672, and continuous compounding at 5% lands at $16,487 — so the extra two points of rate matters more than the compounding cadence.",
        },
      },
    ],
  },
  {
    role: "assistant",
    content: [
      {
        kind: "text",
        data: {
          text:
            "Good question. The short answer: it depends on whether you need transactions across multiple writes.\n\n" +
            "If you do — Postgres. If everything you do is single-row, SQLite is faster, simpler to operate, and easier to back up. A surprising amount of production traffic can live happily on SQLite if you accept its one-writer-at-a-time constraint.\n\n" +
            "Next step: tell me about your read/write ratio and I can be more specific.",
        },
      },
    ],
  },
];

const FALLBACK_REPLY: ChatMessage = {
  role: "assistant",
  content: [{ kind: "text", data: { text: "(No reply available.)" } }],
};

export function pickReply(scriptIndex: number): ChatMessage {
  return (
    SCRIPTED_REPLIES[scriptIndex % SCRIPTED_REPLIES.length] ?? FALLBACK_REPLY
  );
}
