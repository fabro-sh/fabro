import {
  useId,
  useMemo,
  useReducer,
  useState,
  type ReactNode,
} from "react";
import { Link, useParams } from "react-router";
import {
  ArrowDownTrayIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  ClipboardDocumentIcon,
  CpuChipIcon,
} from "@heroicons/react/16/solid";
import { CircleStackIcon, ClockIcon } from "@heroicons/react/20/solid";

import {
  DebugDnaStrip,
  DebugEventDetailsPanel,
  DebugEventRow,
  DetailsPanel,
  EventSearchInput,
  MultiSelectFilter,
  ThreadDnaStrip,
  debugRowCategory,
  threadSelectionId,
  threadSelectionsEqual,
} from "../components/event-debug";
import {
  debugCategory,
  debugCategoryLabel,
  formatElapsed,
  type DebugCategory,
} from "../components/event-debug-helpers";
import type {
  EventDisplayPayload,
  ThreadDnaItem,
  ThreadDnaSelection,
} from "../components/event-debug";
import { StageContext } from "../components/stage-context";
import { StageInsightsSidebar } from "../components/stage-insights-sidebar";
import { StageSidebar } from "../components/stage-sidebar";
import type { Stage } from "../components/stage-sidebar";
import { EmptyState, Spinner } from "../components/state";
import { ToolCallCount } from "../components/tool-call-count";
import {
  HoverCard,
  PopoverHeader,
  PopoverRow,
  PopoverRows,
  Tooltip,
} from "../components/ui";
import { ConditionalDecision } from "../components/stage-renderers/conditional-decision";
import { FanInResults } from "../components/stage-renderers/fan-in-results";
import type {
  EdgeSelection,
  HumanInterviewPair,
  ParallelOverview,
  ReducerTranscript,
  StageContextData,
} from "../components/stage-renderers/helpers";
import { HumanQA } from "../components/stage-renderers/human-qa";
import { ParallelChildren } from "../components/stage-renderers/parallel-children";
import {
  CodeBlock,
  DetailField,
  JsonBlock,
  Markdown,
} from "../components/stage-renderers/primitives";
import { StageSummary } from "../components/stage-renderers/stage-summary";
import { WaitStatus } from "../components/stage-renderers/wait-status";
import {
  formatAbsoluteTs,
  formatBytes,
  formatDurationMs,
  formatTokenCount,
  formatUsdMicros,
} from "../lib/format";
import { costSourceTag, hasUsage, usageTokenBuckets } from "../lib/usage";
import { plural } from "../lib/plural";
import {
  agentEnvelopesOf,
  commandOutcomeOf,
  commandScriptOf,
  outputLossNote,
  debugRowSearchText,
  debugRowsFromStream,
  extractPetriStageContext,
  findPetriEdgeForStage,
  itemsForStage,
  parallelOverviewFromProjection,
  parsePetriInterviewPairs,
  reducerTranscriptFromProjection,
  type CommandOutputLoss,
  type DebugRow,
} from "../lib/petri-stream";
import {
  useRun,
  useRunStageContextWindow,
  useRunStageLog,
  useRunStages,
  useRunState,
  useRunStream,
} from "../lib/queries";
import {
  ACTIVE_STAGE_STATES,
  mapRunStagesToSidebarStages,
} from "../lib/stage-sidebar";
import {
  getNumber,
  getObject,
  getString,
  isRecord,
  type UnknownRecord,
} from "../lib/unknown";
import type {
  ReasoningOutput,
  RunProjection,
  RunStreamItem,
  StageProjection,
  StageHandler,
  StageModelUsage,
  Usage,
} from "@qltysh/fabro-api-client";

export const handle = { wide: true, fullHeight: true };

type TurnType =
  | { kind: "system"; ts: string; content: string }
  | { kind: "steer"; ts: string; content: string }
  | { kind: "interrupt"; ts: string; content: string }
  | { kind: "pair_user"; ts: string; content: string }
  | { kind: "pair_system"; ts: string; content: string }
  | {
      kind: "assistant";
      ts: string;
      content: string;
      inputTokens: number;
      outputTokens: number;
      toolCallCount: number | null;
      reasoning: ReasoningOutput | null;
    }
  | {
      kind: "tool";
      ts: string;
      toolName: string;
      input: string;
      result: string;
      isError: boolean;
      durationMs: number;
    }
  | {
      kind: "command";
      ts: string;
      script: string;
      running: boolean;
      exitCode: number | null;
      durationMs: number;
      outputBytes: number;
      /** What the capture did not keep, or null when the output is whole. */
      outputLoss: CommandOutputLoss | null;
    };

type CommandTurn = Extract<TurnType, { kind: "command" }>;

export type StageRenderer =
  | "agent"
  | "command"
  | "human"
  | "conditional"
  | "parallel"
  | "fan_in"
  | "wait"
  | "summary";

type PanelSelection = ThreadDnaSelection;

export const EVENT_KINDS = [
  "system",
  "steer",
  "interrupt",
  "pair_user",
  "pair_system",
  "assistant",
  "tool",
  "command",
] as const;
export type EventKind = (typeof EVENT_KINDS)[number];

const EVENT_KIND_LABEL: Record<EventKind, string> = {
  system: "System",
  steer: "Steer",
  interrupt: "Interrupt",
  pair_user: "Human",
  pair_system: "System",
  assistant: "Agent",
  tool: "Tool",
  command: "Command",
};

const EVENTS_TABS = ["chat", "primary", "context", "debug"] as const;
type EventsTab = (typeof EVENTS_TABS)[number];

interface StageActivityState {
  tab: EventsTab;
  selectedKinds: EventKind[];
  selectedDebugCategories: DebugCategory[];
  search: string;
}

type StageActivityAction =
  | { type: "tabChanged"; tab: EventsTab }
  | { type: "kindsChanged"; kinds: EventKind[] }
  | { type: "debugCategoriesChanged"; categories: DebugCategory[] }
  | { type: "searchChanged"; search: string };

const initialStageActivityState = (): StageActivityState => ({
  // Only agent stages offer "chat"; every other renderer resolves this to
  // "primary" through `availableTabs`, so this is the default for both.
  tab: "chat",
  selectedKinds: [...EVENT_KINDS],
  selectedDebugCategories: [],
  search: "",
});

function stageActivityReducer(
  state: StageActivityState,
  action: StageActivityAction,
): StageActivityState {
  switch (action.type) {
    case "tabChanged":
      return { ...state, tab: action.tab };
    case "kindsChanged":
      return { ...state, selectedKinds: action.kinds };
    case "debugCategoriesChanged":
      return { ...state, selectedDebugCategories: action.categories };
    case "searchChanged":
      return { ...state, search: action.search };
  }
}

const PRIMARY_TAB_LABEL: Record<StageRenderer, string> = {
  agent: "Thread",
  command: "Logs",
  human: "Q&A",
  conditional: "Decision",
  parallel: "Children",
  fan_in: "Results",
  wait: "Status",
  summary: "Summary",
};

export function eventsTabLabel(
  tab: EventsTab,
  renderer: StageRenderer,
): string {
  if (tab === "chat") return "Chat";
  if (tab === "debug") return "Debug";
  if (tab === "context") return "Context";
  return PRIMARY_TAB_LABEL[renderer];
}

export function selectStageRenderer(handler: StageHandler): StageRenderer {
  switch (handler) {
    case "agent":
    case "prompt":
      return "agent";
    case "command":
      return "command";
    case "human":
      return "human";
    case "conditional":
      return "conditional";
    case "parallel":
      return "parallel";
    case "parallel.fan_in":
      return "fan_in";
    case "wait":
      return "wait";
    default:
      return "summary";
  }
}

interface PendingTool {
  ts: string;
  toolName: string;
  input: string;
}

export interface PendingToolCall {
  toolCallId: string;
  toolName: string;
  input: string;
}

interface StageActivity {
  turns: TurnType[];
  pendingTools: PendingToolCall[];
}

interface PendingCommand {
  ts: string;
  script: string;
}

function readTurnReasoning(props: UnknownRecord): ReasoningOutput | null {
  const reasoning = getObject(props, "reasoning");
  if (!reasoning) return null;
  // getString treats "" as absent, so a provider that sends an empty field
  // reads the same as one that sends nothing.
  const summary = getString(reasoning, "summary") ?? null;
  const trace = getString(reasoning, "trace") ?? null;
  if (summary) return trace ? { summary, trace } : { summary };
  return trace ? { trace } : null;
}

/** The stream and projection of a Petri run, threaded into the stage views. */
export interface PetriRunData {
  stream: RunStreamItem[];
  projection: RunProjection;
}

/**
 * The turns of a stage that ran on Petri: the prompt the projection holds,
 * the Pebble envelopes the stage's step recorded (assistant messages, tool
 * calls, interrupts), and, when no envelope carried the answer, the
 * projection's response as the one assistant turn. A command stage is one
 * command turn from its `step.started` and final `step.finished`.
 */
export function buildPetriStageActivity(
  items: RunStreamItem[],
  stage: StageProjection | undefined,
  renderer: StageRenderer,
): StageActivity {
  const turns: TurnType[] = [];
  const pendingTools = new Map<string, PendingTool>();
  const firstTs = items[0]
    ? new Date(items[0].recorded_at).toISOString()
    : (stage?.started_at ?? new Date(0).toISOString());
  const startTs = stage?.started_at ?? firstTs;

  if (renderer === "command") {
    const started = items.some(
      (item) => item.kind === "petri" && getString(getObject(getObject(item.item, "record"), "body"), "event") === "step.started",
    );
    if (started || stage) {
      const outcome = commandOutcomeOf(items);
      const running =
        stage?.state === "running" || stage?.state === "retrying";
      turns.push({
        kind: "command",
        ts: startTs,
        script: commandScriptOf(items) ?? "",
        running,
        exitCode: outcome.exitCode,
        durationMs: outcome.durationMs || (stage?.timing?.wall_time_ms ?? 0),
        outputBytes: stage?.output_bytes ?? 0,
        outputLoss: outcome.outputLoss,
      });
    }
    return { turns, pendingTools: [] };
  }

  const envelopes = agentEnvelopesOf(items);
  // The prompt is the session's `UserInput`; the projection's `prompt` stands
  // in for a stage whose session recorded none (a prompt node).
  if (stage?.prompt && !envelopes.some((envelope) => envelope.variant === "UserInput")) {
    turns.push({ kind: "system", ts: startTs, content: stage.prompt });
  }
  let sawAssistantMessage = false;
  for (const envelope of envelopes) {
    const { payload } = envelope;
    switch (envelope.variant) {
      case "UserInput": {
        const text = getString(payload, "text") ?? "";
        if (text) turns.push({ kind: "system", ts: envelope.ts, content: text });
        break;
      }
      case "AssistantMessage": {
        sawAssistantMessage = true;
        const tokens = getObject(getObject(payload, "usage"), "tokens") ?? getObject(payload, "usage") ?? {};
        turns.push({
          kind: "assistant",
          ts: envelope.ts,
          content: getString(payload, "text") ?? "",
          inputTokens: getNumber(tokens, "input") ?? 0,
          outputTokens: (getNumber(tokens, "output") ?? 0) + (getNumber(tokens, "reasoning") ?? 0),
          toolCallCount: getNumber(payload, "tool_call_count") ?? null,
          reasoning: readTurnReasoning(payload),
        });
        break;
      }
      case "ToolCallStarted": {
        const callId = getString(payload, "tool_call_id");
        if (!callId) break;
        const args = payload.arguments;
        pendingTools.set(callId, {
          ts: envelope.ts,
          toolName: getString(payload, "tool_name") ?? "",
          input: typeof args === "string" ? args : JSON.stringify(args ?? ""),
        });
        break;
      }
      case "ToolCallCompleted": {
        const callId = getString(payload, "tool_call_id");
        if (!callId) break;
        const started = pendingTools.get(callId);
        pendingTools.delete(callId);
        const output = payload.output ?? "";
        turns.push({
          kind: "tool",
          ts: started?.ts ?? envelope.ts,
          toolName: started?.toolName ?? getString(payload, "tool_name") ?? "",
          input: started?.input ?? "",
          result: typeof output === "string" ? output : JSON.stringify(output, null, 2),
          isError: payload.is_error === true,
          durationMs: durationBetween(started?.ts, envelope.ts),
        });
        break;
      }
      case "SteeringInjected": {
        const text = getString(payload, "text") ?? "";
        if (text) turns.push({ kind: "steer", ts: envelope.ts, content: text });
        break;
      }
      case "RoundInterrupted":
        turns.push({ kind: "interrupt", ts: envelope.ts, content: "Interrupted — waiting for steering" });
        break;
      default:
        break;
    }
  }
  if (!sawAssistantMessage && stage?.response) {
    const tokens = stage.usage?.tokens;
    turns.push({
      kind: "assistant",
      ts: stage.completion?.timestamp ?? firstTs,
      content: stage.response,
      inputTokens: tokens?.input ?? 0,
      outputTokens: tokens?.output ?? 0,
      toolCallCount: null,
      reasoning: null,
    });
  }

  return {
    turns,
    pendingTools: Array.from(pendingTools, ([toolCallId, tool]) => ({
      toolCallId,
      toolName: tool.toolName,
      input: tool.input,
    })),
  };
}

const EMPTY_STREAM: RunStreamItem[] = [];

/** What the details panel shows for a debug row. */
function debugItemPayload(item: DebugRow): EventDisplayPayload {
  return {
    event: item.event,
    stream_seq: item.seq,
    kind: item.item.kind,
    stage: item.stageLabel,
    recorded_at: item.ts,
    item: item.item.item,
  };
}

/** What the Petri renderers show for one stage, derived once per stage. */
interface PetriStageViews {
  pairs: HumanInterviewPair[];
  edge: EdgeSelection | null;
  overview: ParallelOverview;
  reducer: ReducerTranscript | null;
}

type ToolTurn = Extract<TurnType, { kind: "tool" }>;
type ToolGroupChild = { turn: ToolTurn; turnIndex: number };
type ToolGroupChildren = readonly [
  ToolGroupChild,
  ToolGroupChild,
  ...ToolGroupChild[],
];

export type DisplayItem =
  | {
      kind: "single";
      turn: TurnType;
      turnIndex: number;
      selection: Extract<ThreadDnaSelection, { kind: "single" }>;
    }
  | {
      kind: "group";
      toolName: string;
      ts: string;
      durationMs: number;
      children: ToolGroupChildren;
      selection: Extract<ThreadDnaSelection, { kind: "group" }>;
    };

type ChatTurn = Exclude<TurnType, ToolTurn | CommandTurn>;

// The Chat view's projection: messages stay individual turns while
// consecutive tool turns (regardless of tool name) merge into one count.
// Unlike `groupConsecutiveTools`, errored calls stay in the batch — the chip
// reports them as a count instead of breaking the group.
export type ChatItem =
  | { kind: "turn"; turn: ChatTurn; turnIndex: number }
  | { kind: "tools"; ts: string; count: number; errored: number };

export function buildChatItems(turns: TurnType[]): ChatItem[] {
  const out: ChatItem[] = [];
  turns.forEach((turn, turnIndex) => {
    if (turn.kind === "tool") {
      const errored = turn.isError ? 1 : 0;
      const last = out[out.length - 1];
      if (last?.kind === "tools") {
        last.count += 1;
        last.errored += errored;
      } else {
        out.push({ kind: "tools", ts: turn.ts, count: 1, errored });
      }
      return;
    }
    if (turn.kind === "command") return;
    out.push({ kind: "turn", turn, turnIndex });
  });
  return out;
}

// A group's elapsed time is the wall-clock envelope of its children —
// earliest start to latest end — not the sum of their durations. Parallel
// calls overlap, and completion order is not always start order, so neither
// the summed duration nor the last array element is the right answer.
function toolGroupBounds(children: ToolGroupChildren): {
  ts: string;
  durationMs: number;
} {
  let earliestTs = children[0].turn.ts;
  let earliestStart: number | null = null;
  let latestEnd: number | null = null;

  for (const { turn } of children) {
    const startMs = Date.parse(turn.ts);
    if (Number.isNaN(startMs)) continue;
    const endMs = startMs + Math.max(0, turn.durationMs);
    if (earliestStart == null || startMs < earliestStart) {
      earliestStart = startMs;
      earliestTs = turn.ts;
    }
    if (latestEnd == null || endMs > latestEnd) latestEnd = endMs;
  }

  if (earliestStart == null || latestEnd == null) {
    return { ts: earliestTs, durationMs: 0 };
  }
  return { ts: earliestTs, durationMs: Math.max(0, latestEnd - earliestStart) };
}

function singleDisplayItem(turn: TurnType, turnIndex: number): DisplayItem {
  return {
    kind: "single",
    turn,
    turnIndex,
    selection: { kind: "single", turnIndex },
  };
}

function toolGroupDisplayItem(
  first: ToolGroupChild,
  second: ToolGroupChild,
  rest: ToolGroupChild[],
): Extract<DisplayItem, { kind: "group" }> {
  const children: ToolGroupChildren = [first, second, ...rest];
  const bounds = toolGroupBounds(children);
  return {
    kind: "group",
    toolName: first.turn.toolName,
    ts: bounds.ts,
    durationMs: bounds.durationMs,
    children,
    selection: {
      kind: "group",
      childTurnIndices: [
        first.turnIndex,
        second.turnIndex,
        ...rest.map((child) => child.turnIndex),
      ],
    },
  };
}

// Grouping runs over the complete turn stream, never a filtered one: any
// non-tool turn is a real boundary whether or not the current filters make it
// visible, and hiding one must not merge the tool batches on either side.
export function groupConsecutiveTools(
  turns: { turn: TurnType; index: number }[],
): DisplayItem[] {
  const out: DisplayItem[] = [];
  let buf: { turn: ToolTurn; turnIndex: number }[] = [];

  function flush() {
    const [first, second, ...rest] = buf;
    buf = [];
    if (!first) return;
    out.push(
      second
        ? toolGroupDisplayItem(first, second, rest)
        : singleDisplayItem(first.turn, first.turnIndex),
    );
  }

  for (const { turn, index } of turns) {
    const groupable = turn.kind === "tool" && !turn.isError;
    if (
      groupable &&
      (buf.length === 0 || buf[0].turn.toolName === turn.toolName)
    ) {
      buf.push({ turn, turnIndex: index });
      continue;
    }
    flush();
    if (groupable) {
      buf.push({ turn, turnIndex: index });
    } else {
      out.push(singleDisplayItem(turn, index));
    }
  }
  flush();
  return out;
}

// Hide display items that the kind filter or search excludes. This is purely
// a visibility pass: it runs after grouping and after DNA timing, so it can
// never change group membership, timestamps, or durations. A group survives
// when Tool is selected and any child matches the search, and it is passed
// through whole so its context is preserved.
export function filterDisplayItems(
  items: DisplayItem[],
  selectedKinds: readonly EventKind[],
  search: string,
): DisplayItem[] {
  if (
    search.length === 0 &&
    selectedKinds.length === EVENT_KINDS.length &&
    EVENT_KINDS.every((kind) => selectedKinds.includes(kind))
  ) {
    return items;
  }

  const kinds = new Set<string>(selectedKinds);
  const needle = search.toLowerCase();
  const matchesSearch = (turn: TurnType) =>
    !needle || searchableText(turn).toLowerCase().includes(needle);

  return items.filter((item) => {
    if (item.kind === "single") {
      return kinds.has(item.turn.kind) && matchesSearch(item.turn);
    }
    return (
      kinds.has("tool") && item.children.some((c) => matchesSearch(c.turn))
    );
  });
}

export function visibleTurnCount(items: DisplayItem[]): number {
  return items.reduce(
    (total, item) =>
      total + (item.kind === "single" ? 1 : item.children.length),
    0,
  );
}

export function filterThreadDnaItems(
  items: ThreadDnaItem[],
  visibleItems: DisplayItem[],
): ThreadDnaItem[] {
  const visibleIds = new Set(
    visibleItems.map((item) => threadSelectionId(item.selection)),
  );
  return items.filter((item) =>
    visibleIds.has(threadSelectionId(item.selection)),
  );
}

// Convert the complete grouped display list into bars for the Thread DNA
// strip. Each bar carries the same selection identifier the event list uses,
// so clicking a bar opens the same side-panel entry as clicking its row.
//
// Duration semantics:
//   - tool / command turns use their explicit durationMs
//   - tool groups use their wall-clock envelope (see toolGroupBounds)
//   - assistant turns have no native duration; their bar covers the interval
//     from the previous activity's end to the message's ts. That is the
//     inferred model response time — provider queueing, network, streaming,
//     and generation — not a reasoning trace.
//   - system / steer / interrupt are instants (durationMs = 0)
export function buildThreadDnaItems(
  items: DisplayItem[],
  runStart: string | undefined,
): ThreadDnaItem[] {
  if (items.length === 0) return [];

  const anchorMs = (() => {
    if (runStart) {
      const parsed = Date.parse(runStart);
      if (!Number.isNaN(parsed)) return parsed;
    }
    const firstTs = items[0].kind === "single" ? items[0].turn.ts : items[0].ts;
    const parsedFirst = Date.parse(firstTs);
    return Number.isNaN(parsedFirst) ? null : parsedFirst;
  })();
  if (anchorMs == null) return [];

  const out: ThreadDnaItem[] = [];
  let prevEndMs: number | null = null;
  // Overlapping or out-of-order tool completions must never move the
  // previous-activity cursor backward, or the next Agent bar absorbs time
  // that already belonged to a tool.
  const advance = (endMs: number) => {
    prevEndMs = prevEndMs == null ? endMs : Math.max(prevEndMs, endMs);
  };

  for (const item of items) {
    if (item.kind === "single") {
      const turn = item.turn;
      const tsMs = Date.parse(turn.ts);
      if (Number.isNaN(tsMs)) continue;
      const selection = item.selection;

      switch (turn.kind) {
        case "system":
          out.push({
            category: "system",
            label: "stage.prompt",
            startMs: Math.max(0, tsMs - anchorMs),
            durationMs: 0,
            selection,
          });
          advance(tsMs);
          break;
        case "steer":
          out.push({
            category: "user",
            label: "user.steer",
            startMs: Math.max(0, tsMs - anchorMs),
            durationMs: 0,
            selection,
          });
          advance(tsMs);
          break;
        case "interrupt":
          out.push({
            category: "interrupt",
            label: "interrupt",
            startMs: Math.max(0, tsMs - anchorMs),
            durationMs: 0,
            selection,
          });
          advance(tsMs);
          break;
        case "pair_user":
          out.push({
            category: "user",
            label: "pair.user",
            startMs: Math.max(0, tsMs - anchorMs),
            durationMs: 0,
            selection,
          });
          advance(tsMs);
          break;
        case "pair_system":
          out.push({
            category: "system",
            label: "pair.system",
            startMs: Math.max(0, tsMs - anchorMs),
            durationMs: 0,
            selection,
          });
          advance(tsMs);
          break;
        case "assistant": {
          // turn.ts is the moment the assistant message arrived (end of
          // generation). Its bar covers the gap from the last activity to
          // that moment: the model's response time, tool-call-only responses
          // included.
          const startSourceMs = prevEndMs ?? tsMs;
          const startMs = Math.max(0, startSourceMs - anchorMs);
          const durationMs = Math.max(0, tsMs - startSourceMs);
          out.push({
            category: "agent",
            label: "agent.message",
            startMs,
            durationMs,
            selection,
          });
          advance(tsMs);
          break;
        }
        case "tool": {
          const startMs = Math.max(0, tsMs - anchorMs);
          const durationMs = Math.max(0, turn.durationMs);
          out.push({
            category: "tool",
            label: humanizeToolName(turn.toolName),
            startMs,
            durationMs,
            selection,
          });
          advance(tsMs + durationMs);
          break;
        }
        case "command": {
          const startMs = Math.max(0, tsMs - anchorMs);
          const durationMs = Math.max(0, turn.durationMs);
          out.push({
            category: "tool",
            label: "command",
            startMs,
            durationMs,
            selection,
          });
          advance(tsMs + durationMs);
          break;
        }
      }
    } else {
      // item.ts / item.durationMs are already the group's wall-clock
      // envelope, so the row, the details header, and this bar all agree.
      const startTsMs = Date.parse(item.ts);
      if (Number.isNaN(startTsMs)) continue;
      const durationMs = Math.max(0, item.durationMs);
      out.push({
        category: "tool",
        label: `${humanizeToolName(item.toolName)} ×${item.children.length}`,
        startMs: Math.max(0, startTsMs - anchorMs),
        durationMs,
        selection: item.selection,
      });
      advance(startTsMs + durationMs);
    }
  }

  return out;
}

export function formatStageModelUsageLabel(
  providerUsed: StageModelUsage | null | undefined,
): string | null {
  const model = providerUsed?.model;
  if (!model) return null;
  const effort = providerUsed.reasoning_effort;
  return effort ? `${model}[${effort}]` : model;
}

const POPOVER_NUMBER = "block text-right font-mono tabular-nums";

/** Tokens and cost for this stage visit alone. */
function StageUsageRows({ usage }: { usage: Usage }) {
  if (!hasUsage(usage)) return null;
  const buckets = usageTokenBuckets(usage);
  const cost = formatUsdMicros(usage.cost?.usd_micros);
  const costTag = costSourceTag(usage.cost);
  return (
    <div className="mt-3">
      <PopoverHeader>Tokens</PopoverHeader>
      <PopoverRows>
        {buckets.map((bucket) => (
          <PopoverRow key={bucket.label} label={bucket.label}>
            <span className={POPOVER_NUMBER}>
              {bucket.value === 0
                ? "0"
                : formatTokenCount(bucket.value, { compactDecimal: true })}
            </span>
          </PopoverRow>
        ))}
        {cost && (
          <PopoverRow label={costTag ? `Cost (${costTag})` : "Cost"}>
            <span className={POPOVER_NUMBER}>{cost}</span>
          </PopoverRow>
        )}
      </PopoverRows>
    </div>
  );
}

export function ModelUsagePopover({
  providerUsed,
  usage,
}: {
  providerUsed: StageModelUsage;
  usage: Usage;
}) {
  return (
    <>
      <PopoverHeader>Model</PopoverHeader>
      <PopoverRows>
        {providerUsed.provider && (
          <PopoverRow label="Provider">{providerUsed.provider}</PopoverRow>
        )}
        {providerUsed.model && (
          <PopoverRow label="Model">
            <span className="break-all font-mono">{providerUsed.model}</span>
          </PopoverRow>
        )}
        {providerUsed.reasoning_effort && (
          <PopoverRow label="Reasoning">
            {providerUsed.reasoning_effort}
          </PopoverRow>
        )}
        {providerUsed.speed && (
          <PopoverRow label="Speed">{providerUsed.speed}</PopoverRow>
        )}
      </PopoverRows>
      <StageUsageRows usage={usage} />
    </>
  );
}

function turnLabel(turn: TurnType): string {
  switch (turn.kind) {
    case "system":
      return "System";
    case "steer":
      return "Steer";
    case "interrupt":
      return "Interrupt";
    case "pair_user":
      return "Human";
    case "pair_system":
      return "System";
    case "assistant":
      return "Agent";
    case "tool":
      return "Tool";
    case "command":
      return "Command";
  }
}

function turnTone(turn: TurnType): string {
  switch (turn.kind) {
    case "system":
      return "bg-amber/15 text-amber";
    case "steer":
      return "bg-overlay-strong text-fg-2";
    case "interrupt":
      return "bg-coral/15 text-coral";
    case "pair_user":
      return "bg-overlay-strong text-fg-2";
    case "pair_system":
      return "bg-amber/15 text-amber";
    case "assistant":
      return "bg-teal-500/15 text-teal-500";
    case "tool":
    case "command":
      return "bg-mint/15 text-mint";
  }
}

const SUMMARY_MAX_CHARS = 80;

function oneLine(text: string): string {
  const collapsed = text.replace(/\s+/g, " ").trim();
  if (collapsed.length <= SUMMARY_MAX_CHARS) return collapsed;
  return `${collapsed.slice(0, SUMMARY_MAX_CHARS - 1)}…`;
}

function nonBlankAssistantContent(
  turn: Extract<TurnType, { kind: "assistant" }>,
): string | null {
  return turn.content.trim() ? turn.content : null;
}

const TOOL_NAME_DISPLAY: Record<string, string> = {
  read_file: "Read",
  write_file: "Write",
  edit_file: "Edit",
  shell: "Bash",
  grep: "Grep",
  glob: "Glob",
  read_many_files: "Read Many",
  list_dir: "List Dir",
  web_search: "Web Search",
  web_fetch: "Web Fetch",
};

export function humanizeToolName(raw: string): string {
  if (!raw) return "tool";
  if (TOOL_NAME_DISPLAY[raw]) return TOOL_NAME_DISPLAY[raw];
  // MCP tools are namespaced like `mcp__<server>__<tool>`; display the trailing segment.
  const lastSegment = raw.split("__").pop() ?? raw;
  return lastSegment
    .split(/[_-]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

export function turnSummary(turn: TurnType): string {
  switch (turn.kind) {
    case "system":
    case "steer":
    case "interrupt":
    case "pair_user":
    case "pair_system":
      return oneLine(turn.content);
    case "assistant": {
      const line = oneLine(nonBlankAssistantContent(turn) ?? "");
      if (line) return line;
      // A model response that only requested tools has no text of its own;
      // describe what it did instead of rendering a blank row.
      const count = turn.toolCallCount ?? 0;
      if (count > 0) {
        return `Requested ${count} ${plural(count, "tool call", "tool calls")}`;
      }
      return "Model response contained no text";
    }
    case "tool":
      return humanizeToolName(turn.toolName);
    case "command":
      return oneLine(turn.script) || (turn.running ? "running…" : "");
  }
}

function durationBetween(startTs: string | undefined, endTs: string): number {
  if (!startTs) return 0;
  const startMs = Date.parse(startTs);
  const endMs = Date.parse(endTs);
  if (Number.isNaN(startMs) || Number.isNaN(endMs)) return 0;
  return Math.max(0, endMs - startMs);
}

export function turnMetric(turn: TurnType): string | null {
  switch (turn.kind) {
    case "assistant": {
      if (turn.inputTokens === 0 && turn.outputTokens === 0) return null;
      return `${formatTokenCount(turn.inputTokens)} / ${formatTokenCount(turn.outputTokens)}`;
    }
    case "tool":
    case "command":
      return turn.durationMs > 0 ? formatDurationMs(turn.durationMs) : null;
    case "steer":
    case "interrupt":
    case "system":
    case "pair_user":
    case "pair_system":
      return null;
  }
}

export function searchableText(turn: TurnType): string {
  switch (turn.kind) {
    case "system":
    case "steer":
    case "interrupt":
    case "pair_user":
    case "pair_system":
      return turn.content;
    case "assistant": {
      // Text-free responses are findable by the copy the thread shows.
      const content = nonBlankAssistantContent(turn);
      return content ?? turnSummary(turn);
    }
    case "tool":
      return `${humanizeToolName(turn.toolName)} ${turn.toolName} ${turn.input} ${turn.result}`;
    case "command":
      return turn.script;
  }
}

function EventRow({
  turn,
  runStart,
  selected,
  onSelect,
}: {
  turn: TurnType;
  runStart: string | undefined;
  selected: boolean;
  onSelect: () => void;
}) {
  const metric = turnMetric(turn);
  const MetricIcon =
    metric == null
      ? null
      : turn.kind === "assistant"
        ? CircleStackIcon
        : ClockIcon;
  const metricSpan = (
    <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs tabular-nums text-fg-muted">
      {turn.kind === "tool" && turn.isError && (
        <span className="rounded bg-coral/15 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wider text-coral">
          Error
        </span>
      )}
      {MetricIcon && <MetricIcon className="size-3" aria-hidden="true" />}
      {metric ?? ""}
    </span>
  );
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`grid w-full grid-cols-[5rem_1fr_auto_auto] items-center gap-4 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
        selected ? "bg-overlay" : ""
      }`}
    >
      <span
        className={`inline-flex w-fit items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider ${turnTone(turn)}`}
      >
        {turnLabel(turn)}
      </span>
      <span className="min-w-0 truncate text-sm text-fg-3">
        {turnSummary(turn)}
      </span>
      {turn.kind === "assistant" && metric != null ? (
        <Tooltip
          label={
            <div className="p-1">
              <div className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-fg-muted">
                Tokens in / out
              </div>
              <div className="grid grid-cols-[auto_auto] items-baseline gap-x-3 gap-y-1 tabular-nums">
                <span className="text-right font-medium text-fg">
                  {formatTokenCount(turn.inputTokens)}
                </span>
                <span className="text-fg-3">input</span>
                <span className="text-right font-medium text-fg">
                  {formatTokenCount(turn.outputTokens)}
                </span>
                <span className="text-fg-3">output</span>
              </div>
            </div>
          }
        >
          {metricSpan}
        </Tooltip>
      ) : (
        metricSpan
      )}
      <Tooltip label={formatAbsoluteTs(turn.ts)}>
        <span className="pl-3 font-mono text-xs tabular-nums text-fg-muted">
          {formatElapsed(turn.ts, runStart)}
        </span>
      </Tooltip>
    </button>
  );
}

const TOOL_GROUP_TONE = "bg-mint/15 text-mint";

function ToolGroupRow({
  group,
  runStart,
  selected,
  onSelect,
}: {
  group: Extract<DisplayItem, { kind: "group" }>;
  runStart: string | undefined;
  selected: boolean;
  onSelect: () => void;
}) {
  const metric =
    group.durationMs > 0 ? formatDurationMs(group.durationMs) : null;
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`grid w-full grid-cols-[5rem_1fr_auto_auto] items-center gap-4 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
        selected ? "bg-overlay" : ""
      }`}
    >
      <span
        className={`inline-flex w-fit items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider ${TOOL_GROUP_TONE}`}
      >
        Tool
      </span>
      <span className="min-w-0 truncate text-sm text-fg-3">
        {humanizeToolName(group.toolName)} x{group.children.length}
      </span>
      <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs tabular-nums text-fg-muted">
        {metric && <ClockIcon className="size-3" aria-hidden="true" />}
        {metric ?? ""}
      </span>
      <Tooltip label={formatAbsoluteTs(group.ts)}>
        <span className="pl-3 font-mono text-xs tabular-nums text-fg-muted">
          {formatElapsed(group.ts, runStart)}
        </span>
      </Tooltip>
    </button>
  );
}

const COLLAPSIBLE_PREVIEW_CHARS = 280;

/**
 * Shared disclosure for long stage text. By default it preserves raw text;
 * callers may supply a full-content renderer for authored formats such as
 * Markdown while retaining the same plain-text preview and accessible toggle.
 */
function CollapsibleContent({
  text,
  className = "",
  textClassName = "",
  renderFull,
}: {
  text: string;
  className?: string;
  textClassName?: string;
  renderFull?: (text: string) => ReactNode;
}) {
  const [expanded, setExpanded] = useState(false);
  const contentId = useId();
  const isLong = text.length > COLLAPSIBLE_PREVIEW_CHARS;
  const preview = isLong
    ? `${text.slice(0, COLLAPSIBLE_PREVIEW_CHARS).trimEnd()}…`
    : text;

  return (
    <div
      className={`flex flex-col items-start gap-1.5 ${className}`.trim()}
    >
      {/*
        `w-full` is load-bearing for ChatUserCard. Its `w-fit max-w-[85%]`
        bubble is measured intrinsically before being clamped, so this wrapper
        must fill the resolved width to keep prompt text inside the bubble.
      */}
      <div id={contentId} className="w-full">
        {renderFull && (!isLong || expanded) ? (
          renderFull(text)
        ) : (
          <p className={textClassName}>{expanded ? text : preview}</p>
        )}
      </div>
      {isLong && (
        <button
          type="button"
          aria-controls={contentId}
          aria-expanded={expanded}
          onClick={() => setExpanded((current) => !current)}
          className="text-xs text-teal-500 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500"
        >
          {expanded
            ? "Collapse"
            : `Show all (${text.length.toLocaleString()} characters)`}
        </button>
      )}
    </div>
  );
}

export function EventDetails({
  turn,
  runStart,
  hideMeta = false,
}: {
  turn: TurnType;
  runStart: string | undefined;
  hideMeta?: boolean;
}) {
  const elapsed = formatElapsed(turn.ts, runStart);
  const absolute = (() => {
    const ms = Date.parse(turn.ts);
    if (Number.isNaN(ms)) return turn.ts;
    return new Date(ms).toLocaleString();
  })();
  const assistantContent =
    turn.kind === "assistant" ? nonBlankAssistantContent(turn) : null;
  const reasoning = turn.kind === "assistant" ? turn.reasoning : null;
  const reasoningSummary =
    reasoning && "summary" in reasoning ? reasoning.summary : null;

  return (
    <div className="space-y-5">
      {!hideMeta && (
        <DetailField label="When" mono>
          {elapsed ? `${elapsed} · ${absolute}` : absolute}
        </DetailField>
      )}

      {(turn.kind === "system" ||
        turn.kind === "steer" ||
        turn.kind === "interrupt" ||
        turn.kind === "pair_user" ||
        turn.kind === "pair_system") && (
        <DetailField label="Content">
          <Markdown content={turn.content} />
        </DetailField>
      )}

      {turn.kind === "assistant" && (
        <>
          <DetailField label="Content">
            {assistantContent ? (
              <Markdown content={assistantContent} />
            ) : (
              <span className="text-fg-muted">{turnSummary(turn)}</span>
            )}
          </DetailField>
          {/*
            Reasoning follows the message rather than preceding it, the way it
            ran: a trace can be thousands of characters, and leading with one
            would push the answer the user clicked on below the fold.
          */}
          {reasoningSummary && (
            <DetailField label="Reasoning">
              <CollapsibleContent
                text={reasoningSummary}
                textClassName="wrap-break-word whitespace-pre-wrap"
              />
            </DetailField>
          )}
          {reasoning?.trace && (
            <DetailField
              label={reasoningSummary ? "Reasoning trace" : "Reasoning"}
            >
              <CollapsibleContent
                text={reasoning.trace}
                textClassName="wrap-break-word whitespace-pre-wrap"
              />
            </DetailField>
          )}
          {turn.toolCallCount != null && turn.toolCallCount > 0 && (
            <DetailField label="Tool calls" mono>
              {turn.toolCallCount}
            </DetailField>
          )}
          {(turn.inputTokens > 0 || turn.outputTokens > 0) && (
            <DetailField label="Tokens" mono>
              {formatTokenCount(turn.inputTokens)} in ·{" "}
              {formatTokenCount(turn.outputTokens)} out
            </DetailField>
          )}
        </>
      )}

      {turn.kind === "tool" && (
        <>
          {!hideMeta && (
            <DetailField label="Tool" mono>
              {humanizeToolName(turn.toolName)}{" "}
              <span className="text-fg-muted">({turn.toolName})</span>
            </DetailField>
          )}
          <DetailField label="Input">
            <JsonBlock value={turn.input} />
          </DetailField>
          <DetailField label={turn.isError ? "Error" : "Result"}>
            <JsonBlock value={turn.result} />
          </DetailField>
        </>
      )}

      {turn.kind === "command" && (
        <>
          <DetailField label="Status" mono>
            {turn.running
              ? "Running…"
              : `exit ${turn.exitCode ?? "?"}${
                  turn.durationMs
                    ? ` · ${formatDurationMs(turn.durationMs)}`
                    : ""
                }`}
          </DetailField>
          <DetailField label="Script">
            <CodeBlock>{turn.script}</CodeBlock>
          </DetailField>
        </>
      )}
    </div>
  );
}

function decodeBase64Utf8(b64: string): string {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}

function LogStream({
  runId,
  stageId,
  label,
  byteCount,
  enabled,
}: {
  runId: string;
  stageId: string;
  label: string;
  byteCount: number;
  enabled: boolean;
}) {
  const { data, error, isLoading } = useRunStageLog(
    runId,
    stageId,
    enabled && byteCount > 0,
  );
  const text = useMemo(() => {
    if (!data?.bytes_base64) return "";
    try {
      return decodeBase64Utf8(data.bytes_base64);
    } catch {
      return "";
    }
  }, [data]);
  const truncated =
    data && data.total_bytes > data.next_offset
      ? data.total_bytes - data.next_offset
      : 0;

  return (
    <section>
      <header className="mb-1 flex items-baseline justify-between gap-2">
        <h3 className="text-xs font-medium uppercase tracking-wider text-fg-muted">
          {label}
        </h3>
        {byteCount > 0 && (
          <span className="font-mono text-[11px] tabular-nums text-fg-muted">
            {formatBytes(byteCount)}
          </span>
        )}
      </header>
      <pre className="overflow-x-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
        {byteCount === 0 ? (
          <span className="text-fg-muted">empty</span>
        ) : isLoading && !data ? (
          <span className="text-fg-muted">loading…</span>
        ) : error ? (
          <span className="text-coral">Failed to load output.</span>
        ) : (
          text || <span className="text-fg-muted">empty</span>
        )}
      </pre>
      {truncated > 0 && (
        <p className="mt-1 text-[11px] text-fg-muted">
          Showing first {formatBytes(data!.next_offset)} of{" "}
          {formatBytes(data!.total_bytes)}.
        </p>
      )}
    </section>
  );
}

function CommandStatus({ turn }: { turn: CommandTurn }) {
  const exitTone =
    turn.exitCode == null
      ? "text-fg-muted"
      : turn.exitCode === 0
        ? "text-mint"
        : "text-coral";
  return (
    <span className="ml-auto inline-flex items-center gap-x-3 text-xs">
      {turn.running ? (
        <span className="inline-flex items-center gap-1.5 text-amber">
          <span className="size-1.5 animate-pulse rounded-full bg-amber" />
          Running…
        </span>
      ) : (
        <span className={`font-mono tabular-nums ${exitTone}`}>
          exit {turn.exitCode ?? "?"}
        </span>
      )}
      {turn.durationMs > 0 && (
        <span className="font-mono tabular-nums text-fg-muted">
          {formatDurationMs(turn.durationMs)}
        </span>
      )}
    </span>
  );
}

function CommandScript({ script }: { script: string }) {
  return (
    <section>
      <h3 className="mb-1 text-xs font-medium uppercase tracking-wider text-fg-muted">
        Command
      </h3>
      <pre className="overflow-x-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
        {script || <span className="text-fg-muted">empty</span>}
      </pre>
    </section>
  );
}

function CommandLogs({
  runId,
  stageId,
  turn,
}: {
  runId: string;
  stageId: string;
  turn: CommandTurn | null;
}) {
  if (!turn) {
    return (
      <div className="pl-3 pr-4 text-sm text-fg-muted sm:pr-6 lg:pr-8">
        No command output yet.
      </div>
    );
  }
  return (
    <div className="space-y-5 pl-3 pr-4 sm:pr-6 lg:pr-8">
      <CommandScript script={turn.script} />
      <LogStream
        runId={runId}
        stageId={stageId}
        label="Output"
        byteCount={turn.outputBytes}
        enabled={!turn.running}
      />
      {outputLossNote(turn.outputLoss) && (
        <p
          data-testid="command-output-loss"
          className="text-xs text-amber"
        >
          {outputLossNote(turn.outputLoss)}
        </p>
      )}
    </div>
  );
}

function EventDetailsPanel({
  turn,
  runStart,
  onClose,
}: {
  turn: TurnType | null;
  runStart: string | undefined;
  onClose: () => void;
}) {
  return (
    <DetailsPanel
      title={turn ? `${turnLabel(turn)} event` : ""}
      isOpen={turn != null}
      onClose={onClose}
    >
      {turn ? <EventDetails turn={turn} runStart={runStart} /> : null}
    </DetailsPanel>
  );
}

const TOOL_INPUT_PREVIEW_KEYS = [
  "command",
  "path",
  "pattern",
  "url",
  "query",
  "script",
];

function toolInputPreview(raw: string): string {
  if (!raw) return "";
  try {
    const parsed = JSON.parse(raw);
    if (typeof parsed === "string") return oneLine(parsed);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      const obj = parsed as Record<string, unknown>;
      for (const k of TOOL_INPUT_PREVIEW_KEYS) {
        const v = obj[k];
        if (typeof v === "string" && v) return oneLine(v);
      }
    }
  } catch {
    // input wasn't valid JSON; fall through to oneLine of the raw string
  }
  return oneLine(raw);
}

function ToolGroupChildRow({
  child,
  runStart,
  expanded,
  onToggle,
}: {
  child: { turn: ToolTurn; turnIndex: number };
  runStart: string | undefined;
  expanded: boolean;
  onToggle: () => void;
}) {
  const { turn } = child;
  const metric = turn.durationMs > 0 ? formatDurationMs(turn.durationMs) : null;
  const elapsed = formatElapsed(turn.ts, runStart);
  const Chevron = expanded ? ChevronDownIcon : ChevronRightIcon;
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={expanded}
      className={`grid w-full grid-cols-[1fr_auto_auto] items-center gap-3 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
        expanded ? "bg-overlay" : ""
      }`}
    >
      <span className="min-w-0 truncate font-mono text-xs text-fg-3">
        {toolInputPreview(turn.input)}
      </span>
      <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs tabular-nums text-fg-muted">
        {metric && <ClockIcon className="size-3" aria-hidden="true" />}
        {metric ?? ""}
        <Tooltip label={formatAbsoluteTs(turn.ts)}>
          <span className="pl-3 tabular-nums">{elapsed}</span>
        </Tooltip>
      </span>
      <Chevron className="size-4 text-fg-muted" aria-hidden="true" />
    </button>
  );
}

// User-side bubble. The stage prompt (and any steer / pair-user message over
// the preview limit) collapses to a preview with an expand toggle; expanded
// content renders as markdown.
function ChatUserCard({ content }: { content: string }) {
  return (
    <CollapsibleContent
      text={content}
      className="w-fit max-w-[85%] self-end rounded-2xl rounded-br-md bg-panel px-4 py-3"
      textClassName="text-sm wrap-break-word whitespace-pre-wrap text-fg-2"
      renderFull={(text) => <Markdown content={text} />}
    />
  );
}

function ChatLiveToolLine({ tool }: { tool: PendingToolCall }) {
  return (
    <div className="flex items-center gap-2.5 text-xs">
      <Spinner className="size-3 text-teal-500" />
      <span className="shrink-0 text-fg-3">
        {humanizeToolName(tool.toolName)}
      </span>
      <span className="min-w-0 truncate font-mono text-[11px] text-fg-muted">
        {toolInputPreview(tool.input)}
      </span>
    </div>
  );
}

export function StageChatView({
  turns,
  pendingTools,
  stage,
  className = "",
}: {
  turns: TurnType[];
  pendingTools: PendingToolCall[];
  stage: Stage;
  className?: string;
}) {
  const items = useMemo(() => buildChatItems(turns), [turns]);
  const stageActive = ACTIVE_STAGE_STATES.has(stage.status);
  const duration = stage.duration === "--" ? null : stage.duration;
  let lastAssistantTurnIndex: number | null = null;
  for (let i = items.length - 1; i >= 0; i -= 1) {
    const item = items[i];
    if (item.kind === "turn" && item.turn.kind === "assistant") {
      lastAssistantTurnIndex = item.turnIndex;
      break;
    }
  }
  return (
    <div className={`flex w-full max-w-2xl flex-col gap-4 px-5 ${className}`}>
      {items.length === 0 && !stageActive && (
        <p className="py-6 text-sm text-fg-muted">
          No agent activity recorded for this stage.
        </p>
      )}
      {items.map((item, i) => {
        if (item.kind === "tools") {
          return (
            <ToolCallCount
              key={`tools-${item.ts}-${i}`}
              count={item.count}
              errored={item.errored}
            />
          );
        }
        const { turn, turnIndex } = item;
        switch (turn.kind) {
          case "system":
          case "steer":
          case "pair_user":
            return (
              <ChatUserCard key={`turn-${turnIndex}`} content={turn.content} />
            );
          case "pair_system":
            return (
              <p
                key={`turn-${turnIndex}`}
                className="text-xs whitespace-pre-wrap text-fg-muted"
              >
                {turn.content}
              </p>
            );
          case "interrupt":
            return (
              <p key={`turn-${turnIndex}`} className="text-xs text-coral">
                {turn.content}
              </p>
            );
          case "assistant": {
            const assistantContent = nonBlankAssistantContent(turn);
            const metric = turnMetric(turn);
            const isFinal =
              turnIndex === lastAssistantTurnIndex && !stageActive;
            const showFooter = isFinal && Boolean(metric || duration);
            // A text-free assistant turn is the boundary between two batches
            // of tool calls, kept in the turn stream so those batches stay
            // separate chips. It has nothing to show, and an empty node would
            // still take a slot in this gap-4 column, doubling the space
            // between the chips on either side of it.
            if (!assistantContent && !showFooter) return null;
            return (
              <div key={`turn-${turnIndex}`} className="flex flex-col gap-1.5">
                {assistantContent && <Markdown content={assistantContent} />}
                {showFooter && (
                  <div className="flex gap-3 font-mono text-[11px] text-fg-muted tabular-nums">
                    {metric && <span>{metric}</span>}
                    {duration && <span>{duration}</span>}
                  </div>
                )}
              </div>
            );
          }
        }
      })}
      {stageActive && pendingTools.length > 0 && (
        <div role="status" aria-live="polite" className="flex flex-col gap-2.5">
          <span className="sr-only">
            {pendingTools.length} tool{" "}
            {plural(pendingTools.length, "call", "calls")} in progress
          </span>
          {pendingTools.map((tool) => (
            <ChatLiveToolLine key={tool.toolCallId} tool={tool} />
          ))}
        </div>
      )}
    </div>
  );
}

function ToolGroupDetails({
  group,
  runStart,
}: {
  group: Extract<DisplayItem, { kind: "group" }>;
  runStart: string | undefined;
}) {
  const [expandedIndex, setExpandedIndex] = useState<number | null>(null);

  const elapsed = formatElapsed(group.ts, runStart);
  const totalDuration =
    group.durationMs > 0 ? formatDurationMs(group.durationMs) : null;

  return (
    <div className="-mx-5 -mt-4">
      <div className="flex items-baseline gap-3 border-b border-line px-5 py-3">
        <span className="text-sm font-medium text-fg">
          {humanizeToolName(group.toolName)}{" "}
          <span className="text-fg-muted">x{group.children.length}</span>
        </span>
        <span className="ml-auto inline-flex items-center gap-1.5 font-mono text-xs tabular-nums text-fg-muted">
          {elapsed}
          {totalDuration && (
            <>
              <span aria-hidden="true">·</span>
              <ClockIcon className="size-3" aria-hidden="true" />
              {totalDuration}
            </>
          )}
        </span>
      </div>
      <ul className="divide-y divide-line">
        {group.children.map((child, i) => (
          <li key={`group-child-${child.turnIndex}`}>
            <ToolGroupChildRow
              child={child}
              runStart={runStart}
              expanded={expandedIndex === i}
              onToggle={() =>
                setExpandedIndex((current) => (current === i ? null : i))
              }
            />
            {expandedIndex === i && (
              <div className="bg-overlay/50 px-5 py-4">
                <EventDetails turn={child.turn} runStart={runStart} hideMeta />
              </div>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function ToolGroupDetailsPanel({
  group,
  runStart,
  onClose,
}: {
  group: Extract<DisplayItem, { kind: "group" }> | null;
  runStart: string | undefined;
  onClose: () => void;
}) {
  const detailsKey = group
    ? `tool-group-details-${group.children.map((child) => child.turnIndex).join("-")}`
    : "empty";

  return (
    <DetailsPanel
      title={group ? "Tool group" : ""}
      isOpen={group != null}
      onClose={onClose}
    >
      {group ? (
        <ToolGroupDetails key={detailsKey} group={group} runStart={runStart} />
      ) : null}
    </DetailsPanel>
  );
}

function EventsTabToggle({
  tab,
  renderer,
  availableTabs,
  onTabChange,
}: {
  tab: EventsTab;
  renderer: StageRenderer;
  availableTabs: readonly EventsTab[];
  onTabChange: (tab: EventsTab) => void;
}) {
  return (
    <div className="inline-flex rounded-md bg-panel p-0.5 outline-1 -outline-offset-1 outline-line-strong">
      {availableTabs.map((value) => {
        const active = tab === value;
        return (
          <button
            key={value}
            type="button"
            onClick={() => onTabChange(value)}
            aria-pressed={active}
            className={`rounded px-2.5 py-1 text-xs font-medium transition-colors focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500 ${
              active
                ? "bg-overlay-strong text-fg"
                : "text-fg-muted hover:text-fg-2"
            }`}
          >
            {eventsTabLabel(value, renderer)}
          </button>
        );
      })}
    </div>
  );
}

function EventExportActions({
  events,
  runId,
  stageId,
  className,
}: {
  events: RunStreamItem[];
  runId: string;
  stageId: string;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);
  const disabled = events.length === 0;
  const buttonClass =
    "inline-flex size-6 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-2 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-fg-muted";
  return (
    <div className={`flex items-center gap-1 ${className ?? ""}`}>
      <button
        type="button"
        disabled={disabled}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(
              JSON.stringify(events, null, 2),
            );
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1200);
          } catch {
            // ignore — clipboard may be unavailable in some contexts
          }
        }}
        title={copied ? "Copied!" : "Copy loaded events as JSON"}
        aria-label="Copy loaded events as JSON"
        className={buttonClass}
      >
        <ClipboardDocumentIcon className="size-3.5" aria-hidden="true" />
      </button>
      <button
        type="button"
        disabled={disabled}
        onClick={() => {
          const jsonl = events.map((e) => JSON.stringify(e)).join("\n");
          const blob = new Blob([jsonl], { type: "application/x-ndjson" });
          const url = URL.createObjectURL(blob);
          const a = document.createElement("a");
          a.href = url;
          a.download = `${runId}-${stageId}-events.jsonl`;
          document.body.appendChild(a);
          a.click();
          a.remove();
          URL.revokeObjectURL(url);
        }}
        title="Download loaded events as JSONL"
        aria-label="Download loaded events as JSONL"
        className={buttonClass}
      >
        <ArrowDownTrayIcon className="size-3.5" aria-hidden="true" />
      </button>
    </div>
  );
}

function EventsToolbar({
  tab,
  renderer,
  availableTabs,
  commandTurn,
  onTabChange,
  selectedKinds,
  onKindsChange,
  selectedDebugCategories,
  onDebugCategoriesChange,
  availableDebugCategories,
  search,
  onSearchChange,
  filteredCount,
  totalCount,
  providerUsed,
  usage,
  events,
  runId,
  stageId,
}: {
  tab: EventsTab;
  renderer: StageRenderer;
  availableTabs: readonly EventsTab[];
  commandTurn: CommandTurn | null;
  onTabChange: (tab: EventsTab) => void;
  selectedKinds: EventKind[];
  onKindsChange: (kinds: EventKind[]) => void;
  selectedDebugCategories: DebugCategory[];
  onDebugCategoriesChange: (categories: DebugCategory[]) => void;
  availableDebugCategories: readonly DebugCategory[];
  search: string;
  onSearchChange: (value: string) => void;
  filteredCount: number;
  totalCount: number;
  providerUsed: StageModelUsage | null;
  usage: Usage;
  events: RunStreamItem[];
  runId: string;
  stageId: string;
}) {
  // Filters apply to: the agent transcript (filter event kinds) and the Debug
  // tab (filter event categories). Specialized renderers (human, parallel,
  // wait, etc.) and the command logs view don't have a filterable list.
  const showFilters =
    tab === "debug" || (tab === "primary" && renderer === "agent");
  const transcriptAllSelected = selectedKinds.length === EVENT_KINDS.length;
  const debugAllSelected =
    selectedDebugCategories.length === 0 ||
    selectedDebugCategories.length === availableDebugCategories.length;
  const isFiltering =
    showFilters &&
    (tab === "primary"
      ? !transcriptAllSelected || search.length > 0
      : !debugAllSelected || search.length > 0);

  function clearFilters() {
    if (tab === "primary") onKindsChange([...EVENT_KINDS]);
    else onDebugCategoriesChange([]);
    onSearchChange("");
  }

  const modelUsageLabel = useMemo(
    () => formatStageModelUsageLabel(providerUsed),
    [providerUsed],
  );

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-2 pb-3">
      <EventsTabToggle
        tab={tab}
        renderer={renderer}
        availableTabs={availableTabs}
        onTabChange={onTabChange}
      />
      {showFilters && (
        <div className="flex flex-1 flex-wrap items-center gap-2">
          {tab === "primary" ? (
            <MultiSelectFilter<EventKind>
              selected={selectedKinds}
              options={EVENT_KINDS}
              labelOf={(k) => EVENT_KIND_LABEL[k]}
              onChange={onKindsChange}
            />
          ) : (
            <MultiSelectFilter<DebugCategory>
              selected={selectedDebugCategories}
              options={availableDebugCategories}
              labelOf={debugCategoryLabel}
              onChange={onDebugCategoriesChange}
              emptyMeansAll
            />
          )}
          <EventSearchInput value={search} onChange={onSearchChange} />
          {isFiltering && (
            <button
              type="button"
              onClick={clearFilters}
              className="rounded px-2 py-1 text-xs text-fg-muted transition-colors hover:bg-overlay hover:text-fg-2 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500"
            >
              Clear
            </button>
          )}
        </div>
      )}
      {totalCount > 0 && (tab === "debug" || isFiltering) && (
        <span className="text-xs tabular-nums text-fg-muted">
          {isFiltering
            ? `${filteredCount.toLocaleString()} of ${totalCount.toLocaleString()} events`
            : `${totalCount.toLocaleString()} events`}
        </span>
      )}
      {modelUsageLabel && providerUsed && (
        <HoverCard
          className={`inline-flex items-center gap-1.5 text-xs text-fg-muted ${
            showFilters ? "" : "ml-auto"
          }`}
          content={
            <ModelUsagePopover providerUsed={providerUsed} usage={usage} />
          }
        >
          <CpuChipIcon className="size-3.5" aria-hidden="true" />
          <span className="font-mono">{modelUsageLabel}</span>
        </HoverCard>
      )}
      {tab === "debug" && (
        <EventExportActions events={events} runId={runId} stageId={stageId} />
      )}
      {tab === "primary" && renderer === "command" && commandTurn && (
        <CommandStatus turn={commandTurn} />
      )}
    </div>
  );
}

function StageActivityBody({
  effectiveTab,
  renderer,
  activity,
  visibleItems,
  panelSelection,
  onPanelSelectionChange,
  runStart,
  runId,
  selectedStage,
  commandTurn,
  debugEvents,
  filteredDebugEvents,
  openDebugSeq,
  onDebugSeqChange,
  contextData,
  stages,
  petri,
}: {
  effectiveTab: EventsTab;
  renderer: StageRenderer;
  activity: StageActivity;
  visibleItems: DisplayItem[];
  panelSelection: PanelSelection | null;
  onPanelSelectionChange: (selection: PanelSelection | null) => void;
  runStart: string | undefined;
  runId: string;
  selectedStage: Stage;
  commandTurn: CommandTurn | null;
  debugEvents: DebugRow[];
  filteredDebugEvents: DebugRow[];
  openDebugSeq: number | null;
  onDebugSeqChange: (seq: number | null) => void;
  contextData: StageContextData | null;
  stages: Stage[];
  /** What the stage's renderers show, derived from the stream and projection. */
  petri: PetriStageViews;
}) {
  const { turns, pendingTools } = activity;
  return (
    <div className="min-h-0 flex-1 overflow-y-auto pt-6 pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]">
      {effectiveTab === "chat" ? (
        <StageChatView
          key={selectedStage.id}
          turns={turns}
          pendingTools={pendingTools}
          stage={selectedStage}
          className="mx-auto"
        />
      ) : effectiveTab === "primary" ? (
        renderer === "agent" ? (
          turns.length > 0 && visibleItems.length === 0 ? (
            <div className="px-2 py-6 text-sm text-fg-muted">
              No events match these filters.
            </div>
          ) : (
            visibleItems.map((item) => {
              const selection = item.selection;
              const selectionId = threadSelectionId(selection);
              const isSelected = threadSelectionsEqual(
                selection,
                panelSelection,
              );
              if (item.kind === "single") {
                return (
                  <EventRow
                    key={selectionId}
                    turn={item.turn}
                    runStart={runStart}
                    selected={isSelected}
                    onSelect={() => onPanelSelectionChange(selection)}
                  />
                );
              }
              return (
                <ToolGroupRow
                  key={selectionId}
                  group={item}
                  runStart={runStart}
                  selected={isSelected}
                  onSelect={() => onPanelSelectionChange(selection)}
                />
              );
            })
          )
        ) : renderer === "command" ? (
          <CommandLogs
            runId={runId}
            stageId={selectedStage.id}
            turn={commandTurn}
          />
        ) : renderer === "human" ? (
          <HumanQA stage={selectedStage} pairs={petri.pairs} />
        ) : renderer === "conditional" ? (
          <ConditionalDecision
            stage={selectedStage}
            edge={petri.edge}
            allStages={stages}
            runId={runId}
          />
        ) : renderer === "parallel" ? (
          <ParallelChildren
            stage={selectedStage}
            overview={petri.overview}
            runId={runId}
            allStages={stages}
          />
        ) : renderer === "fan_in" ? (
          <FanInResults stage={selectedStage} reducer={petri.reducer} />
        ) : renderer === "wait" ? (
          <WaitStatus stage={selectedStage} />
        ) : (
          <StageSummary stage={selectedStage} events={debugEvents} />
        )
      ) : effectiveTab === "context" ? (
        contextData ? (
          <StageContext data={contextData} />
        ) : null
      ) : debugEvents.length > 0 && filteredDebugEvents.length === 0 ? (
        <div className="px-2 py-6 text-sm text-fg-muted">
          No events match these filters.
        </div>
      ) : (
        filteredDebugEvents.map((event) => (
          <DebugEventRow
            key={`debug-${event.seq}`}
            event={event}
            runStart={runStart}
            selected={openDebugSeq === event.seq}
            onSelect={() => onDebugSeqChange(event.seq)}
          />
        ))
      )}
    </div>
  );
}

function RunStageActivityStage({
  runId,
  selectedStage,
  stages,
  runStart,
  tab,
  selectedKinds,
  selectedDebugCategories,
  search,
  onTabChange,
  onKindsChange,
  onDebugCategoriesChange,
  onSearchChange,
  petri,
}: {
  runId: string;
  selectedStage: Stage;
  stages: Stage[];
  runStart: string | undefined;
  tab: EventsTab;
  selectedKinds: EventKind[];
  selectedDebugCategories: DebugCategory[];
  search: string;
  onTabChange: (tab: EventsTab) => void;
  onKindsChange: (kinds: EventKind[]) => void;
  onDebugCategoriesChange: (categories: DebugCategory[]) => void;
  onSearchChange: (search: string) => void;
  /** The run's stream and projection when it executes on Petri. */
  petri?: PetriRunData;
}) {
  const selectedStageId = selectedStage.id;
  const renderer: StageRenderer = selectStageRenderer(selectedStage.handler);
  // The stage's views read the run's stream and projection; until those
  // load, the stage shows as empty.
  const stream = petri?.stream ?? EMPTY_STREAM;
  const stageItems = useMemo<RunStreamItem[]>(
    () => itemsForStage(stream, selectedStageId),
    [stream, selectedStageId],
  );
  const stageProjection: StageProjection | undefined =
    petri?.projection.stages[selectedStageId];
  const activity = useMemo(
    () => buildPetriStageActivity(stageItems, stageProjection, renderer),
    [stageItems, stageProjection, renderer],
  );
  const { turns } = activity;
  const debugEvents = useMemo<DebugRow[]>(
    () => debugRowsFromStream(stageItems),
    [stageItems],
  );
  const petriViews = useMemo<PetriStageViews>(
    () => ({
      pairs: parsePetriInterviewPairs(stageItems),
      edge: findPetriEdgeForStage(stream, selectedStageId),
      overview: parallelOverviewFromProjection(stageProjection),
      reducer: reducerTranscriptFromProjection(stageProjection),
    }),
    [stream, stageItems, stageProjection, selectedStageId],
  );
  // The Context tab surfaces the workflow's deliberate per-visit outputs. It
  // only exists when the stage completed and actually wrote something.
  const contextData = useMemo(
    () => extractPetriStageContext(stageItems),
    [stageItems],
  );
  const availableTabs = useMemo<EventsTab[]>(
    () =>
      EVENTS_TABS.filter((candidate) => {
        if (candidate === "chat") return renderer === "agent";
        if (candidate === "context") return contextData != null;
        return true;
      }),
    [renderer, contextData],
  );
  const effectiveTab: EventsTab = availableTabs.includes(tab) ? tab : "primary";
  const isPrimaryAgent = effectiveTab === "primary" && renderer === "agent";
  const isDebug = effectiveTab === "debug";

  const commandTurn = useMemo<CommandTurn | null>(() => {
    if (effectiveTab !== "primary" || renderer !== "command") return null;
    for (let i = turns.length - 1; i >= 0; i -= 1) {
      const t = turns[i];
      if (t.kind === "command") return t;
    }
    return null;
  }, [effectiveTab, renderer, turns]);

  const [panelSelection, setPanelSelection] = useState<PanelSelection | null>(
    null,
  );
  const [openDebugSeq, setOpenDebugSeq] = useState<number | null>(null);
  // Semantics first, visibility second: grouping and DNA timing are derived
  // from the complete turn stream, and the kind/search filters only decide
  // which of those items are shown.
  const displayItems = useMemo(
    () =>
      isPrimaryAgent
        ? groupConsecutiveTools(turns.map((turn, index) => ({ turn, index })))
        : [],
    [isPrimaryAgent, turns],
  );
  const visibleItems = useMemo(
    () =>
      isPrimaryAgent
        ? filterDisplayItems(displayItems, selectedKinds, search)
        : [],
    [isPrimaryAgent, displayItems, selectedKinds, search],
  );
  const visibleCount = useMemo(
    () => (isPrimaryAgent ? visibleTurnCount(visibleItems) : 0),
    [isPrimaryAgent, visibleItems],
  );
  const allDnaItems = useMemo(
    () => (isPrimaryAgent ? buildThreadDnaItems(displayItems, runStart) : []),
    [isPrimaryAgent, displayItems, runStart],
  );
  const threadDnaItems = useMemo(
    () =>
      !isPrimaryAgent
        ? []
        : visibleItems === displayItems
          ? allDnaItems
          : filterThreadDnaItems(allDnaItems, visibleItems),
    [isPrimaryAgent, allDnaItems, displayItems, visibleItems],
  );

  const openTurn =
    isPrimaryAgent && panelSelection?.kind === "single"
      ? (turns[panelSelection.turnIndex] ?? null)
      : null;
  // Resolve against the complete group list so changing a filter cannot
  // corrupt or drop the identity of an open selection.
  const openGroup = useMemo<Extract<
    DisplayItem,
    { kind: "group" }
  > | null>(() => {
    if (!isPrimaryAgent || panelSelection?.kind !== "group") return null;
    for (const item of displayItems) {
      if (
        item.kind === "group" &&
        threadSelectionsEqual(item.selection, panelSelection)
      ) {
        return item;
      }
    }
    return null;
  }, [isPrimaryAgent, displayItems, panelSelection]);
  const openDebugEvent = useMemo<EventDisplayPayload | null>(() => {
    if (!isDebug || openDebugSeq == null) return null;
    const item = debugEvents.find((e) => e.seq === openDebugSeq);
    return item ? debugItemPayload(item) : null;
  }, [isDebug, debugEvents, openDebugSeq]);
  const availableDebugCategories = useMemo<DebugCategory[]>(() => {
    if (!isDebug) return [];
    const set = new Set<DebugCategory>();
    for (const event of debugEvents) {
      if (event.event) set.add(debugRowCategory(event));
    }
    return Array.from(set).sort();
  }, [isDebug, debugEvents]);
  const filteredDebugEvents = useMemo<DebugRow[]>(() => {
    if (!isDebug) return [];
    const useCategoryFilter = selectedDebugCategories.length > 0;
    const cats = new Set(selectedDebugCategories);
    const needle = search.toLowerCase();
    return debugEvents.filter((event) => {
      if (useCategoryFilter && !cats.has(debugRowCategory(event))) return false;
      if (needle && !debugRowSearchText(event).includes(needle)) return false;
      return true;
    });
  }, [isDebug, debugEvents, selectedDebugCategories, search]);

  return (
    <>
      <div className="flex min-h-0 min-w-0 flex-1 flex-col pt-3">
        <div className="shrink-0 border-b border-line">
          <div className="pl-3 pr-3">
            {selectedStage.resumedFromStageId && (
              <p className="pb-2 text-xs text-fg-muted">
                Resumed from{" "}
                <Link
                  to={`/runs/${runId}/stages/${encodeURIComponent(selectedStage.resumedFromStageId)}`}
                  className="font-mono text-teal-500 hover:underline"
                >
                  {selectedStage.resumedFromStageId}
                </Link>
              </p>
            )}
            <EventsToolbar
              tab={effectiveTab}
              renderer={renderer}
              availableTabs={availableTabs}
              commandTurn={commandTurn}
              onTabChange={onTabChange}
              selectedKinds={selectedKinds}
              onKindsChange={onKindsChange}
              selectedDebugCategories={selectedDebugCategories}
              onDebugCategoriesChange={onDebugCategoriesChange}
              availableDebugCategories={availableDebugCategories}
              search={search}
              onSearchChange={onSearchChange}
              filteredCount={
                effectiveTab === "primary"
                  ? visibleCount
                  : filteredDebugEvents.length
              }
              totalCount={
                effectiveTab === "primary" ? turns.length : debugEvents.length
              }
              providerUsed={selectedStage.providerUsed}
              usage={selectedStage.usage}
              events={stageItems}
              runId={runId}
              stageId={selectedStageId}
            />
            {effectiveTab === "debug" && (
              <div className="pb-3">
                <DebugDnaStrip
                  events={debugEvents}
                  selectedSeq={openDebugSeq}
                  onSelect={setOpenDebugSeq}
                  runStart={runStart}
                />
              </div>
            )}
            {effectiveTab === "primary" && renderer === "agent" && (
              <div className="pb-3">
                <ThreadDnaStrip
                  items={threadDnaItems}
                  selection={panelSelection}
                  onSelect={setPanelSelection}
                />
              </div>
            )}
          </div>
        </div>
        <StageActivityBody
          effectiveTab={effectiveTab}
          renderer={renderer}
          activity={activity}
          visibleItems={visibleItems}
          panelSelection={panelSelection}
          onPanelSelectionChange={setPanelSelection}
          runStart={runStart}
          runId={runId}
          selectedStage={selectedStage}
          commandTurn={commandTurn}
          debugEvents={debugEvents}
          filteredDebugEvents={filteredDebugEvents}
          openDebugSeq={openDebugSeq}
          onDebugSeqChange={setOpenDebugSeq}
          contextData={contextData}
          stages={stages}
          petri={petriViews}
        />
      </div>

      {effectiveTab === "primary" && renderer === "agent" ? (
        panelSelection?.kind === "group" ? (
          <ToolGroupDetailsPanel
            group={openGroup}
            runStart={runStart}
            onClose={() => setPanelSelection(null)}
          />
        ) : (
          <EventDetailsPanel
            turn={openTurn}
            runStart={runStart}
            onClose={() => setPanelSelection(null)}
          />
        )
      ) : effectiveTab === "debug" ? (
        <DebugEventDetailsPanel
          event={openDebugEvent}
          onClose={() => setOpenDebugSeq(null)}
        />
      ) : null}
    </>
  );
}

function RunStageActivity({
  runId,
  selectedStage,
  stages,
  runStart,
  petri,
}: {
  runId: string;
  selectedStage: Stage;
  stages: Stage[];
  runStart: string | undefined;
  petri?: PetriRunData;
}) {
  const [activityState, dispatchActivity] = useReducer(
    stageActivityReducer,
    undefined,
    initialStageActivityState,
  );
  const { tab, selectedKinds, selectedDebugCategories, search } = activityState;

  return (
    <RunStageActivityStage
      key={selectedStage.id}
      runId={runId}
      selectedStage={selectedStage}
      stages={stages}
      runStart={runStart}
      tab={tab}
      selectedKinds={selectedKinds}
      selectedDebugCategories={selectedDebugCategories}
      search={search}
      onTabChange={(nextTab) =>
        dispatchActivity({ type: "tabChanged", tab: nextTab })
      }
      onKindsChange={(kinds) =>
        dispatchActivity({ type: "kindsChanged", kinds })
      }
      onDebugCategoriesChange={(categories) =>
        dispatchActivity({
          type: "debugCategoriesChanged",
          categories,
        })
      }
      onSearchChange={(nextSearch) =>
        dispatchActivity({ type: "searchChanged", search: nextSearch })
      }
      petri={petri}
    />
  );
}

export default function RunStages() {
  const { id, stageId } = useParams();
  const runQuery = useRun(id);
  const stagesQuery = useRunStages(id);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );

  const selectedStage =
    stages.find((s: Stage) => s.id === stageId) ?? stages[0];
  const selectedStageId = selectedStage?.id;
  const runStart =
    selectedStage?.startedAt ??
    runQuery.data?.timestamps.started_at ??
    runQuery.data?.timestamps.created_at;
  // The stage views read the projection and the run's stream; the
  // projection also feeds the insights sidebar of an agent stage, and the
  // context window is fetched only for one.
  const isAgentStage = selectedStage?.handler === "agent";
  const runStateQuery = useRunState(id);
  const streamQuery = useRunStream(id);
  const petriData = useMemo<PetriRunData | undefined>(
    () =>
      runStateQuery.data && streamQuery.data
        ? { stream: streamQuery.data, projection: runStateQuery.data }
        : undefined,
    [runStateQuery.data, streamQuery.data],
  );
  const contextWindowQuery = useRunStageContextWindow(
    isAgentStage ? id : undefined,
    isAgentStage ? selectedStageId : undefined,
  );
  const stageProjection =
    isAgentStage && selectedStageId
      ? runStateQuery.data?.stages[selectedStageId]
      : undefined;

  if (!id || !selectedStage) {
    return (
      <div className="py-12">
        <EmptyState
          title="No stages yet"
          description="Stages will appear here once the run begins executing."
        />
      </div>
    );
  }

  return (
    <div className="-mr-4 -mt-3 flex min-h-0 flex-1 sm:-mr-6 lg:-mr-8">
      <div className="min-h-0 shrink-0 overflow-y-auto overflow-x-hidden pr-3 pt-3 pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]">
        <StageSidebar
          stages={stages}
          runId={id}
          selectedStageId={selectedStage.id}
        />
      </div>

      <div className="relative w-px shrink-0">
        <div
          aria-hidden="true"
          className="absolute inset-x-0 top-0 -bottom-6 bg-line"
        />
      </div>

      {isAgentStage && (
        <>
          <div className="min-h-0 shrink-0 overflow-y-auto overflow-x-hidden px-3 pt-3 pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]">
            <StageInsightsSidebar
              stage={stageProjection}
              contextWindow={contextWindowQuery.data}
            />
          </div>
          <div className="relative w-px shrink-0">
            <div
              aria-hidden="true"
              className="absolute inset-x-0 top-0 -bottom-6 bg-line"
            />
          </div>
        </>
      )}

      <RunStageActivity
        runId={id}
        selectedStage={selectedStage}
        stages={stages}
        runStart={runStart}
        petri={petriData}
      />
    </div>
  );
}
