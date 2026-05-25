import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";

import {
  AgentSkillActivationSource,
  AgentToolCategory,
  StageContextWindowCategory,
  StageContextWindowCountMethod,
  StageContextWindowStaleness,
  TodoListKind,
  TodoStatus,
} from "@qltysh/fabro-api-client";
import type {
  StageContextWindow,
  StageProjection,
} from "@qltysh/fabro-api-client";

import { StageInsightsSidebar } from "./stage-insights-sidebar";

function makeStage(overrides: Partial<StageProjection> = {}): StageProjection {
  return {
    first_event_seq: 1,
    state:           "running",
    usage:           {
      input_tokens:        0,
      output_tokens:       0,
      cache_read_tokens:   0,
      cache_create_tokens: 0,
      total_tokens:        0,
    } as StageProjection["usage"],
    ...overrides,
  };
}

function makeContextWindow(overrides: Partial<StageContextWindow> = {}): StageContextWindow {
  return {
    stage_id:              "implement@1",
    available:             true,
    unavailable_reason:    null,
    provider:              "anthropic",
    model:                 "claude-opus-4-7",
    context_window_tokens: 200_000,
    input_tokens:          62_000,
    usage_percent:         31,
    count_method:          StageContextWindowCountMethod.PROVIDER_API_SCALED_BREAKDOWN,
    staleness:             StageContextWindowStaleness.LIVE,
    generated_at:          new Date().toISOString(),
    event_seq:             42,
    breakdown:             [
      { category: StageContextWindowCategory.SYSTEM_PROMPT, tokens: 8_000, usage_percent: 4 },
      { category: StageContextWindowCategory.TOOLS, tokens: 12_000, usage_percent: 6 },
      { category: StageContextWindowCategory.CONVERSATION, tokens: 42_000, usage_percent: 21 },
    ],
    warnings: [],
    ...overrides,
  };
}

// bun:test runs in a node-like env without a DOM, so shim `window.localStorage`
// once — the sidebar feature-detects `typeof window` to decide whether to
// persist collapse state. Seeding the shim lets us open default-collapsed
// sections (Skills, MCPs) in the assertions below. Other test files (e.g.
// services-panel.test.tsx) install their own window and rely on
// `delete globalThis.window` cleanup, so this descriptor stays configurable.
let restoreWindow: (() => void) | null = null;
beforeAll(() => {
  const store = new Map<string, string>();
  for (const key of ["todos", "context", "tools", "skills", "mcps"]) {
    store.set(`fabro:stage-insights-section:${key}`, "1");
  }
  const stub = {
    localStorage: {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => {
        store.set(key, value);
      },
    },
  };
  const had = "window" in globalThis;
  const prev = (globalThis as { window?: unknown }).window;
  Object.defineProperty(globalThis, "window", { value: stub, writable: true, configurable: true });
  restoreWindow = () => {
    if (had) {
      Object.defineProperty(globalThis, "window", { value: prev, writable: true, configurable: true });
    } else {
      delete (globalThis as { window?: unknown }).window;
    }
  };
});

afterAll(() => {
  restoreWindow?.();
  restoreWindow = null;
});

function render(stage: StageProjection | undefined, contextWindow: StageContextWindow | null): string {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter>
        <StageInsightsSidebar stage={stage} contextWindow={contextWindow} />
      </MemoryRouter>,
    );
  });
  return JSON.stringify(renderer.toJSON());
}

describe("StageInsightsSidebar", () => {
  test("renders todo done/total ratio", () => {
    const stage = makeStage({
      todos: {
        kind:    TodoListKind.ANTHROPIC_TASKS,
        list_id: "anthropic_tasks:root",
        items:   [
          { id: "1", status: TodoStatus.COMPLETED, order: 0, subject: "Plan refactor" },
          { id: "2", status: TodoStatus.COMPLETED, order: 1, subject: "Add tests" },
          { id: "3", status: TodoStatus.IN_PROGRESS, order: 2, subject: "Land migration" },
          { id: "4", status: TodoStatus.PENDING, order: 3, subject: "Review with Kieran" },
        ],
      },
    });
    const dom = render(stage, null);
    expect(dom).toContain("2/4");
    expect(dom).toContain("Plan refactor");
    expect(dom).toContain("Land migration");
  });

  test("renders context window percent and breakdown labels", () => {
    const dom = render(makeStage(), makeContextWindow());
    expect(dom).toContain("31%");
    expect(dom).toContain("System prompt");
    expect(dom).toContain("Conversation");
  });

  test("hides breakdown labels in unavailable state but still renders bar", () => {
    const cw = makeContextWindow({
      available:          false,
      usage_percent:      null,
      input_tokens:       null,
      staleness:          StageContextWindowStaleness.UNAVAILABLE,
      unavailable_reason: null,
    });
    const dom = render(makeStage(), cw);
    expect(dom).toContain("--");
    expect(dom).not.toContain("31%");
  });

  test("renders projected agent tool names and invoked state", () => {
    const dom = render(
      makeStage({
        agent_tools: [
          {
            name:        "apply_patch",
            description: "Apply a unified diff patch",
            source:      { kind: "native" },
            category:    AgentToolCategory.WRITE,
            invoked:     true,
          },
          {
            name:        "grep",
            description: "Search file contents",
            source:      { kind: "native" },
            category:    AgentToolCategory.READ,
            invoked:     false,
          },
        ],
      }),
      null,
    );

    expect(dom).toContain("1/2");
    expect(dom).toContain("apply_patch");
    expect(dom).toContain("grep");
    // Tool description still appears as the row `title` tooltip.
    expect(dom).toContain("Apply a unified diff patch");
    expect(dom).toContain("Search file contents");
  });

  test("renders mcp server used/total count, marks invoked servers as 'used'", () => {
    const dom = render(
      makeStage({
        mcp_servers: [
          {
            server_name: "context7",
            tool_count:  12,
            status:      { kind: "ready", tools: [] },
            invoked:     true,
          },
          {
            server_name: "filesystem",
            tool_count:  3,
            status:      { kind: "ready", tools: [] },
            invoked:     false,
          },
          {
            server_name: "atlassian",
            tool_count:  0,
            status:      { kind: "failed", error: "auth failed" },
            invoked:     false,
          },
        ],
      }),
      null,
    );
    // Used count badge in the section header (1 of 3 invoked).
    expect(dom).toContain("1/3");
    expect(dom).toContain("context7");
    expect(dom).toContain("used");
    expect(dom).toContain("filesystem");
    expect(dom).toContain("3 tools");
    expect(dom).toContain("atlassian");
    expect(dom).toContain("Failed");
  });

  test("shows skill activated/available ratio with source label", () => {
    const dom = render(
      makeStage({
        skills: {
          activated: [
            { name: "frontend-design", source: AgentSkillActivationSource.SLASH },
            { name: "debug", source: AgentSkillActivationSource.TOOL },
          ],
          available: [
            { name: "frontend-design", description: "" },
            { name: "debug", description: "" },
            { name: "tdd", description: "" },
            { name: "ce-review", description: "" },
          ],
        },
      }),
      null,
    );
    expect(dom).toContain("2/4");
    expect(dom).toContain("frontend-design");
    expect(dom).toContain("slash");
    expect(dom).toContain("+2 more available");
  });

  test("renders empty-friendly content when stage projection is missing", () => {
    const dom = render(undefined, null);
    // sidebar still renders even with no data
    expect(dom).toContain("Agent");
  });
});
