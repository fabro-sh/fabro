import { useEffect, useMemo, useRef, useState } from "react";
import { useParams } from "react-router";
import { Marked } from "marked";

const SAFE_HTTP_URL_RE = /^https?:\/\//i;
const SAFE_MAILTO_URL_RE = /^mailto:/i;

export function isSafeMarkdownHref(href: string): boolean {
  return (
    SAFE_HTTP_URL_RE.test(href) ||
    SAFE_MAILTO_URL_RE.test(href) ||
    href.startsWith("#") ||
    (href.startsWith("/") && !href.startsWith("//"))
  );
}

const markedSafe = new Marked();
markedSafe.use({
  async: false,
  walkTokens(token) {
    if (
      (token.type === "link" || token.type === "image") &&
      typeof token.href === "string" &&
      !isSafeMarkdownHref(token.href)
    ) {
      token.href = "";
    }
  },
  renderer: {
    html() {
      return "";
    },
  },
});
import { CommandLineIcon, ChatBubbleLeftIcon, PlayIcon } from "@heroicons/react/24/outline";
import { ToolBlock } from "../components/tool-use";
import type { ToolUse } from "../components/tool-use";
import { StageSidebar, statusConfig } from "../components/stage-sidebar";
import type { Stage } from "../components/stage-sidebar";
import { EmptyState } from "../components/state";
import { CopyButton } from "../components/ui";
import { formatDurationSecs } from "../lib/format";
import { fetchRunCommandLog, useRunStageEvents, useRunStages } from "../lib/queries";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";
import { getNumber, getString, type UnknownRecord } from "../lib/unknown";
import {
  CommandOutputStream,
  CommandTermination,
  type EventEnvelope,
} from "@qltysh/fabro-api-client";

export const handle = { wide: true };

type TurnType =
  | { kind: "system"; content: string }
  | { kind: "assistant"; content: string }
  | { kind: "tool"; tools: ToolUse[] }
  | { kind: "command"; stageId: string; script: string; language: string; stdout?: string; stderr?: string; exitCode?: number | null; durationMs?: number; termination?: CommandTermination; running: boolean };

function readTermination(props: UnknownRecord): CommandTermination {
  const v = props.termination;
  if (v === CommandTermination.EXITED || v === CommandTermination.TIMED_OUT || v === CommandTermination.CANCELLED) {
    return v;
  }
  return CommandTermination.EXITED;
}

export function eventsToActivity(events: EventEnvelope[], stageId: string): TurnType[] {
  const stageEvents = events.filter((e) => e.node_id === stageId);
  const turns: TurnType[] = [];
  // Collect tool pairs: started → completed
  const pendingTools = new Map<string, { toolName: string; input: string }>();
  // Track pending command for pairing started → completed
  let pendingCommand: { stageId: string; script: string; language: string } | undefined;

  for (const e of stageEvents) {
    const props = e.properties ?? {};
    switch (e.event) {
      case "stage.prompt":
        turns.push({ kind: "system", content: getString(props, "text") ?? e.text ?? "" });
        break;
      case "agent.message": {
        const msg = getString(props, "text") ?? e.text ?? "";
        if (msg) turns.push({ kind: "assistant", content: msg });
        break;
      }
      case "agent.tool.started": {
        const callId = getString(props, "tool_call_id") ?? e.tool_call_id ?? "";
        const args = props.arguments ?? e.arguments;
        pendingTools.set(callId, {
          toolName: getString(props, "tool_name") ?? e.tool_name ?? "",
          input: typeof args === "string" ? args : JSON.stringify(args ?? ""),
        });
        break;
      }
      case "agent.tool.completed": {
        const callId = getString(props, "tool_call_id") ?? e.tool_call_id ?? "";
        const started = pendingTools.get(callId);
        const output = props.output ?? e.output ?? "";
        const result = typeof output === "string" ? output : JSON.stringify(output);
        const tool: ToolUse = {
          id: callId,
          toolName: started?.toolName ?? getString(props, "tool_name") ?? e.tool_name ?? "",
          input: started?.input ?? "",
          result,
          isError: (props.is_error ?? e.is_error) === true,
        };
        pendingTools.delete(callId);
        turns.push({ kind: "tool", tools: [tool] });
        break;
      }
      case "command.started": {
        pendingCommand = {
          stageId: e.stage_id ?? `${stageId}@1`,
          script: getString(props, "script") ?? "",
          language: getString(props, "language") ?? "shell",
        };
        break;
      }
      case "command.completed": {
        turns.push({
          kind: "command",
          stageId: pendingCommand?.stageId ?? e.stage_id ?? `${stageId}@1`,
          script: pendingCommand?.script ?? "",
          language: pendingCommand?.language ?? "shell",
          stdout: getString(props, "stdout") ?? "",
          stderr: getString(props, "stderr") ?? "",
          exitCode: getNumber(props, "exit_code") ?? null,
          durationMs: getNumber(props, "duration_ms") ?? 0,
          termination: readTermination(props),
          running: false,
        });
        pendingCommand = undefined;
        break;
      }
    }
  }

  // If command.started was seen but no command.completed, it's still running
  if (pendingCommand) {
    turns.push({
      kind: "command",
      stageId: pendingCommand.stageId,
      script: pendingCommand.script,
      language: pendingCommand.language,
      running: true,
    });
  }

  return turns;
}

function Markdown({ content }: { content: string }) {
  const html = useMemo(() => markedSafe.parse(content, { async: false }) as string, [content]);
  return (
    <div
      className="prose prose-sm max-w-none text-fg-3 prose-headings:text-fg-2 prose-strong:text-fg-2 prose-code:rounded prose-code:bg-overlay-strong prose-code:px-1 prose-code:py-0.5 prose-code:text-[0.8em] prose-code:font-mono prose-code:text-fg-3 prose-code:before:content-none prose-code:after:content-none prose-pre:bg-overlay-strong prose-pre:text-fg-3 prose-a:text-teal-500"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

function SystemBlock({ content }: { content: string }) {
  return (
    <section className="group relative border-l-2 border-amber/50 pl-4">
      <header className="mb-1.5 flex items-center gap-2">
        <CommandLineIcon className="size-4 shrink-0 text-amber" />
        <span className="text-xs font-medium text-fg-3">System prompt</span>
        <div className="ml-auto opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
          <CopyButton value={content} label="Copy system prompt" />
        </div>
      </header>
      <Markdown content={content} />
    </section>
  );
}

function AssistantBlock({ content }: { content: string }) {
  return (
    <section className="group relative border-l-2 border-teal-500/50 pl-4">
      <header className="mb-1.5 flex items-center gap-2">
        <ChatBubbleLeftIcon className="size-4 shrink-0 text-teal-500" />
        <span className="text-xs font-medium text-fg-3">Assistant</span>
        <div className="ml-auto opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
          <CopyButton value={content} label="Copy assistant message" />
        </div>
      </header>
      <Markdown content={content} />
    </section>
  );
}

function StatusPill({
  tone,
  children,
}: {
  tone: "running" | "failed" | "success" | "neutral";
  children: React.ReactNode;
}) {
  const toneClass = {
    running: "bg-teal-500/15 text-teal-500",
    failed: "bg-coral/15 text-coral",
    success: "bg-mint/15 text-mint",
    neutral: "bg-overlay text-fg-3",
  }[tone];
  return (
    <span className={`rounded px-1.5 py-0.5 text-[11px] font-medium ${toneClass}`}>
      {children}
    </span>
  );
}

const COLLAPSE_AFTER_LINES = 20;
const LOG_POLL_INTERVAL_MS = 1000;
const LOG_FETCH_LIMIT_BYTES = 65_536;
const LOG_MEMORY_CAP_BYTES = 5 * 1024 * 1024;

function StreamLabel({ label }: { label: string }) {
  return (
    <div className="font-mono text-[11px] uppercase tracking-wider text-fg-muted">
      {label}
    </div>
  );
}

interface CommandLogState {
  text: string;
  eof: boolean;
  loading: boolean;
  error: boolean;
  truncated: boolean;
  casRef: string | null;
  liveStreaming: boolean;
  totalBytes: number;
}

function decodeBase64Bytes(value: string): Uint8Array {
  if (!value) return new Uint8Array();
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function trimTextToBytes(text: string, maxBytes: number) {
  // Each UTF-16 code unit encodes to at most 3 bytes in UTF-8 (4-byte encodings
  // come from surrogate pairs counted as 2 units). Skip the full encode when
  // the upper bound is already under the cap.
  if (text.length * 3 <= maxBytes) {
    return { text, truncated: false };
  }
  const encoded = new TextEncoder().encode(text);
  if (encoded.byteLength <= maxBytes) {
    return { text, truncated: false };
  }
  const start = encoded.byteLength - maxBytes;
  const trimmed = new TextDecoder().decode(encoded.slice(start));
  return { text: trimmed.replace(/^\uFFFD/, ""), truncated: true };
}

function useCommandLog(
  runId: string | undefined,
  stageId: string | undefined,
  stream: CommandOutputStream,
  running: boolean,
): CommandLogState {
  const [state, setState] = useState<CommandLogState>({
    text: "",
    eof: false,
    loading: true,
    error: false,
    truncated: false,
    casRef: null,
    liveStreaming: false,
    totalBytes: 0,
  });
  const offsetRef = useRef(0);
  const finalPollDoneRef = useRef(false);
  const decoderRef = useRef(new TextDecoder());

  useEffect(() => {
    offsetRef.current = 0;
    finalPollDoneRef.current = false;
    decoderRef.current = new TextDecoder();
    setState({
      text: "",
      eof: false,
      loading: true,
      error: false,
      truncated: false,
      casRef: null,
      liveStreaming: false,
      totalBytes: 0,
    });
  }, [runId, stageId, stream]);

  useEffect(() => {
    if (!runId || !stageId) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    async function poll() {
      try {
        const chunk = await fetchRunCommandLog(
          runId,
          stageId,
          stream,
          offsetRef.current,
          LOG_FETCH_LIMIT_BYTES,
        );
        if (cancelled) return;

        offsetRef.current = chunk.next_offset;
        const bytes = decodeBase64Bytes(chunk.bytes_base64);
        const decoded = decoderRef.current.decode(bytes, { stream: !chunk.eof });
        finalPollDoneRef.current = chunk.eof;
        setState((current) => {
          if (
            decoded.length === 0 &&
            current.eof === chunk.eof &&
            current.totalBytes === chunk.total_bytes &&
            current.casRef === chunk.cas_ref &&
            current.liveStreaming === chunk.live_streaming &&
            !current.loading &&
            !current.error
          ) {
            return current;
          }
          const next = trimTextToBytes(current.text + decoded, LOG_MEMORY_CAP_BYTES);
          return {
            text: next.text,
            eof: chunk.eof,
            loading: false,
            error: false,
            truncated: current.truncated || next.truncated,
            casRef: chunk.cas_ref,
            liveStreaming: chunk.live_streaming,
            totalBytes: chunk.total_bytes,
          };
        });
      } catch {
        if (!cancelled) {
          setState((current) => ({ ...current, loading: false, error: true }));
        }
      }

      if (!cancelled && (running || !finalPollDoneRef.current)) {
        timer = setTimeout(poll, LOG_POLL_INTERVAL_MS);
      }
    }

    void poll();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [runId, running, stageId, stream]);

  return state;
}

function streamStatus(state: CommandLogState, hasContent: boolean): string {
  if (state.error) return "Failed to load";
  if (state.loading) return "Waiting";
  if (hasContent) {
    if (state.eof) return state.casRef ? "Stored" : "Complete";
    return state.liveStreaming ? "Streaming" : "Running";
  }
  return state.eof ? "No output" : "Waiting";
}

function OutputStream({
  label,
  state,
  tone = "normal",
  forceExpanded = false,
}: {
  label: string;
  state: CommandLogState;
  tone?: "normal" | "error";
  forceExpanded?: boolean;
}) {
  const content = state.text;
  const lines = content.split("\n");
  const isLong = lines.length > COLLAPSE_AFTER_LINES;
  const [expanded, setExpanded] = useState(forceExpanded);
  const scrollRef = useRef<HTMLPreElement>(null);
  const followTailRef = useRef(true);
  const visible = isLong && !expanded
    ? lines.slice(-COLLAPSE_AFTER_LINES).join("\n")
    : content;
  const hiddenLines = isLong && !expanded ? lines.length - COLLAPSE_AFTER_LINES : 0;
  const preClass =
    tone === "error"
      ? "whitespace-pre-wrap font-mono text-sm leading-relaxed text-coral sm:text-xs"
      : "whitespace-pre-wrap font-mono text-sm leading-relaxed text-fg-3 sm:text-xs";
  const status = streamStatus(state, content.length > 0);

  useEffect(() => {
    if (!forceExpanded) return;
    setExpanded(true);
  }, [forceExpanded]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el && followTailRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [visible]);

  return (
    <div>
      <div className="mb-1 flex items-center gap-2">
        <StreamLabel label={label} />
        <span className="text-[11px] text-fg-muted">{status}</span>
        {state.truncated ? (
          <span className="text-[11px] text-amber">Last 5 MiB</span>
        ) : null}
        <CopyButton
          value={visible}
          label={`Copy ${label}`}
          className="-my-1"
        />
      </div>
      {isLong && !expanded ? (
        <button
          type="button"
          onClick={() => setExpanded(true)}
          className="mb-2 text-[11px] font-medium text-teal-500 hover:text-teal-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 rounded"
        >
          Show {hiddenLines} earlier lines
        </button>
      ) : null}
      {content.length === 0 ? (
        <div className="font-mono text-sm text-fg-muted sm:text-xs">
          {state.error ? "Unable to fetch this stream." : "No bytes received yet."}
        </div>
      ) : (
        <pre
          ref={scrollRef}
          onScroll={(event) => {
            const el = event.currentTarget;
            followTailRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
          }}
          className={`${preClass} max-h-96 overflow-auto`}
        >
          {visible}
        </pre>
      )}
    </div>
  );
}

function CommandBlock({
  runId,
  turn,
}: {
  runId: string | undefined;
  turn: Extract<TurnType, { kind: "command" }>;
}) {
  const failed = !turn.running && (turn.termination !== CommandTermination.EXITED || turn.exitCode !== 0);
  const stdout = useCommandLog(runId, turn.stageId, CommandOutputStream.STDOUT, turn.running);
  const stderr = useCommandLog(runId, turn.stageId, CommandOutputStream.STDERR, turn.running);
  const borderColor = turn.running ? "border-teal-500/20" : failed ? "border-coral/15" : "border-mint/15";
  const bgColor = turn.running ? "bg-teal-500/5" : failed ? "bg-coral/5" : "bg-mint/5";

  return (
    <div className={`group rounded-md border ${borderColor} ${bgColor} overflow-hidden`}>
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2">
        <PlayIcon className={`size-4 shrink-0 ${turn.running ? "text-teal-500 animate-pulse" : failed ? "text-coral" : "text-mint"}`} />
        <span className="text-xs font-medium text-fg-3">
          {turn.language === "python" ? "Python" : "Shell"}
        </span>
        <div className="ml-auto flex items-center gap-2">
          {turn.running ? (
            <StatusPill tone="running">Running…</StatusPill>
          ) : turn.termination === CommandTermination.TIMED_OUT ? (
            <StatusPill tone="failed">Timed out</StatusPill>
          ) : turn.termination === CommandTermination.CANCELLED ? (
            <StatusPill tone="failed">Cancelled</StatusPill>
          ) : (
            <>
              <StatusPill tone={failed ? "failed" : "success"}>
                exit {turn.exitCode ?? "?"}
              </StatusPill>
              {turn.durationMs != null && (
                <StatusPill tone="neutral">
                  {turn.durationMs < 1000
                    ? `${turn.durationMs}ms`
                    : `${(turn.durationMs / 1000).toFixed(1)}s`}
                </StatusPill>
              )}
            </>
          )}
          {turn.script ? (
            <div className="opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
              <CopyButton value={turn.script} label="Copy script" />
            </div>
          ) : null}
        </div>
      </div>

      {/* Script */}
      {turn.script && (
        <div className="border-t border-line px-3 py-2.5">
          <pre className="whitespace-pre-wrap font-mono text-sm leading-relaxed text-fg-3 sm:text-xs">{turn.script}</pre>
        </div>
      )}

      <div className="grid border-t border-line md:grid-cols-2">
        <div className="border-line px-3 py-2.5 md:border-r">
          <OutputStream label="stdout" state={stdout} />
        </div>
        <div className="border-t border-line px-3 py-2.5 md:border-t-0">
          <OutputStream
            label="stderr"
            state={stderr}
            tone="error"
            forceExpanded={failed || stderr.text.length > 0}
          />
        </div>
      </div>
    </div>
  );
}

function RunningStageDuration({
  isRunning,
  duration,
}: {
  isRunning: boolean;
  duration: string;
}) {
  const [startedAt, setStartedAt] = useState<number | null>(() =>
    isRunning ? Date.now() : null,
  );
  const [, setTick] = useState(0);

  useEffect(() => {
    setStartedAt((current) => {
      if (!isRunning) return null;
      return current ?? Date.now();
    });
  }, [isRunning]);

  useEffect(() => {
    if (!isRunning) return;
    const interval = setInterval(() => setTick((tick) => tick + 1), 1000);
    return () => clearInterval(interval);
  }, [isRunning]);

  if (isRunning && startedAt) {
    return formatDurationSecs(Math.floor((Date.now() - startedAt) / 1000));
  }
  return duration;
}

export default function RunStages() {
  const { id, stageId } = useParams();
  const stagesQuery = useRunStages(id);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );

  const selectedStage = stages.find((s: Stage) => s.id === stageId) ?? stages[0];
  const stageEventsQuery = useRunStageEvents(id, selectedStage?.id);
  const turns = useMemo(
    () =>
      selectedStage
        ? eventsToActivity(stageEventsQuery.data ?? [], selectedStage.id)
        : [],
    [stageEventsQuery.data, selectedStage],
  );
  const isRunning = selectedStage?.status === "running";

  if (!id || !stages.length) {
    return (
      <div className="py-12">
        <EmptyState
          title="No stages yet"
          description="Stages will appear here once the run begins executing."
        />
      </div>
    );
  }

  const selectedConfig = statusConfig[selectedStage.status];
  const SelectedIcon = selectedConfig.icon;

  return (
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id} selectedStageId={selectedStage.id} />

      <div className="min-w-0 flex-1 space-y-3">
        <div className="sticky top-0 z-10 -mx-2 flex items-center gap-2 bg-page/85 px-2 py-2 backdrop-blur">
          <SelectedIcon className={`size-5 ${selectedConfig.color} ${isRunning ? "animate-spin" : ""}`} />
          <h3 className="text-base font-semibold text-fg">{selectedStage.name}</h3>
          <span className="font-mono text-xs tabular-nums text-fg-muted">
            <RunningStageDuration
              isRunning={isRunning}
              duration={selectedStage.duration}
            />
          </span>
        </div>

        {turns.map((turn: TurnType, i: number) => {
          switch (turn.kind) {
            case "system":
              return <SystemBlock key={`turn-${i}`} content={turn.content} />;
            case "assistant":
              return <AssistantBlock key={`turn-${i}`} content={turn.content} />;
            case "tool":
              return <ToolBlock key={`turn-${i}`} tools={turn.tools} />;
            case "command":
              return <CommandBlock key={`turn-${i}`} runId={id} turn={turn} />;
          }
        })}
      </div>
    </div>
  );
}