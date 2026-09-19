import { createElement, type ReactNode } from "react";
import type { RunStreamItem } from "@qltysh/fabro-api-client";
import TestRenderer, { act } from "react-test-renderer";

import type { Stage } from "./stage-sidebar";
import { makeUsage } from "./test-fixtures";
import type { UnknownRecord } from "./unknown";

const IS_REACT_ACT_ENV = "IS_REACT_ACT_ENVIRONMENT" as const;

/**
 * Per-test setup for code that uses react-test-renderer:
 * - Sets IS_REACT_ACT_ENVIRONMENT (required by act()).
 * - Silences react-test-renderer's deprecation warning.
 *
 * Returns a teardown function; pair with beforeEach/afterEach so the global
 * state is scoped to the test rather than leaking process-wide.
 */
export function setupReactTestEnv(): () => void {
  type Globals = { [IS_REACT_ACT_ENV]?: boolean };
  const globals = globalThis as Globals;
  const hadEnv = IS_REACT_ACT_ENV in globals;
  const previousEnv = globals[IS_REACT_ACT_ENV];
  globals[IS_REACT_ACT_ENV] = true;

  const originalConsoleError = console.error;
  console.error = ((...args: unknown[]) => {
    if (
      typeof args[0] === "string" &&
      args[0].startsWith("react-test-renderer is deprecated")
    ) {
      return;
    }
    originalConsoleError(...args);
  }) as typeof console.error;

  return () => {
    console.error = originalConsoleError;
    if (hadEnv) {
      globals[IS_REACT_ACT_ENV] = previousEnv;
    } else {
      delete globals[IS_REACT_ACT_ENV];
    }
  };
}

const STREAM_EPOCH_MS = Date.parse("2026-04-09T12:00:00Z");

/** The stage a Petri item's subject names: a node, its visit and its firing. */
export interface StreamStage {
  name: string;
  visit?: number;
  firing?: number;
  /** The node's `meta.kind`; `agent` unless given. */
  kind?: string;
}

/**
 * A platform record item of a run's stream, as `GET /runs/{id}/events`
 * serves it: `record.kind` names the fact. Recorded one second per `seq`
 * after the stream epoch unless `recorded_at` is overridden.
 */
export function makePlatformItem(
  seq: number,
  record: UnknownRecord & { kind: string },
  overrides: Partial<RunStreamItem> = {},
): RunStreamItem {
  const recordedAt = overrides.recorded_at ?? STREAM_EPOCH_MS + seq * 1000;
  return {
    run_id: "run-1",
    stream_seq: seq,
    kind: "platform",
    id: String(seq),
    recorded_at: recordedAt,
    item: { seq, recorded_at: recordedAt, record },
    ...overrides,
  };
}

/**
 * A Petri event item of a run's stream: the recorded `body` (named by its
 * `event`) under the stage `subject`, with Petri's `derived` view beside it.
 */
export function makePetriItem(
  seq: number,
  body: UnknownRecord & { event: string },
  {
    stage,
    derived,
    ...overrides
  }: { stage?: StreamStage; derived?: UnknownRecord } & Partial<RunStreamItem> = {},
): RunStreamItem {
  const recordedAt = overrides.recorded_at ?? STREAM_EPOCH_MS + seq * 1000;
  const subject = stage
    ? {
        node: { id: 0, name: stage.name, kind: "attractor/stage", meta: { kind: stage.kind ?? "agent" } },
        firing: stage.firing ?? stage.visit ?? 1,
        visit: stage.visit ?? 1,
        attempt: 1,
      }
    : undefined;
  return {
    run_id: "run-1",
    stream_seq: seq,
    kind: "petri",
    id: `execution 0/${seq}/0`,
    recorded_at: recordedAt,
    item: {
      id: { log: "execution", execution: 0, seq, index: 0 },
      origin: "external",
      context: { invocation: 0, execution: 0 },
      ...(subject ? { subject } : {}),
      recorded_at: recordedAt,
      record: { seq, origin: "external", recorded_at: recordedAt, body },
      ...(derived ? { derived } : {}),
    },
    ...overrides,
  };
}

/**
 * A stage's `step.progress.recorded` carrying one Pebble `CodingAgentEvent`
 * envelope: the `variant` (`AssistantMessage`, `ToolCallStarted`, ...) with
 * its `payload`, stamped `ts`.
 */
export function makePebbleItem(
  seq: number,
  ts: string,
  stage: StreamStage,
  variant: string,
  payload: UnknownRecord,
): RunStreamItem {
  return makePetriItem(
    seq,
    {
      event: "step.progress.recorded",
      firing: stage.firing ?? stage.visit ?? 1,
      ev: {
        custom: {
          kind: "pebble",
          node: stage.name,
          event: {
            seq,
            stream_id: "ses_1",
            session_id: "ses_1",
            timestamp: ts,
            event: { [variant]: payload },
          },
        },
      },
    },
    { stage, recorded_at: Date.parse(ts) },
  );
}

/** Flatten a rendered subtree to its visible text. */
export function textContent(node: TestRenderer.ReactTestInstance): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textContent(child)))
    .join("");
}

/**
 * Build a sidebar `Stage` fixture; override any field via `overrides`. Kept
 * here so widening `Stage` updates every fixture at once — test files are
 * excluded from typecheck, so a per-file copy silently goes stale instead.
 */
export function makeStage(overrides: Partial<Stage> = {}): Stage {
  return {
    id: "implement@1",
    name: "implement",
    handler: "agent",
    nodeId: "implement",
    visit: 1,
    graphVisit: null,
    resumedFromStageId: null,
    parallelGroupId: null,
    parallelBranchIndex: null,
    status: "running",
    duration: "--",
    startedAt: null,
    providerUsed: null,
    usage: makeUsage(),
    ...overrides,
  };
}

export function renderHook<T>(
  hook: () => T,
  options: { wrapper: React.ComponentType<{ children: ReactNode }> },
): { result: { current: T } } {
  const result = { current: undefined as unknown as T };
  function HookHost() {
    result.current = hook();
    return null;
  }
  act(() => {
    TestRenderer.create(
      createElement(options.wrapper, null, createElement(HookHost)),
    );
  });
  return { result };
}
