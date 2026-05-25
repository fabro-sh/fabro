import { useMemo } from "react";
import {
  ArrowPathIcon,
  ChatBubbleLeftEllipsisIcon,
  CheckCircleIcon,
  ClockIcon,
  ExclamationTriangleIcon,
  NoSymbolIcon,
} from "@heroicons/react/20/solid";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import type { Stage } from "../stage-sidebar";
import { Tooltip } from "../ui";
import { formatAbsoluteTs, formatDurationMs } from "../../lib/format";
import { ACTIVE_STAGE_STATES } from "../../lib/stage-sidebar";
import { Markdown } from "./primitives";
import { StageMetaBar } from "./meta-bar";
import {
  parseHumanInterviewPairs,
  type HumanInterviewPair,
  type HumanResolution,
  type InterviewOption,
} from "./helpers";

function questionTypeLabel(type: string): string {
  switch (type) {
    case "multiple_choice":
    case "MultipleChoice":
      return "Multiple choice";
    case "yes_no":
    case "YesNo":
      return "Yes / No";
    case "freeform":
    case "Freeform":
      return "Freeform";
    default:
      return type.replace(/_/g, " ");
  }
}

function answerLookup(options: InterviewOption[], answer: string): InterviewOption | null {
  const trimmed = answer.trim();
  return options.find((o) => o.key === trimmed || o.label === trimmed) ?? null;
}

function ResolutionBlock({
  resolution,
  options,
}: {
  resolution: HumanResolution;
  options: InterviewOption[];
}) {
  if (resolution.kind === "answered") {
    const matched = answerLookup(options, resolution.answer);
    return (
      <div className="rounded-lg bg-teal-500/5 p-4 outline-1 -outline-offset-1 outline-teal-500/20">
        <div className="mb-2 flex items-center gap-2 text-xs">
          <CheckCircleIcon className="size-3.5 text-teal-500" aria-hidden="true" />
          <span className="font-medium uppercase tracking-wider text-teal-500">
            Answered
          </span>
          {resolution.actor && (
            <span className="text-fg-muted">
              by <span className="font-mono text-fg-3">{resolution.actor}</span>
            </span>
          )}
          <span className="ml-auto inline-flex items-center gap-1 font-mono tabular-nums text-fg-muted">
            <ClockIcon className="size-3" aria-hidden="true" />
            {formatDurationMs(resolution.durationMs)}
          </span>
        </div>
        {matched ? (
          <div className="flex items-baseline gap-3">
            <span className="font-mono text-xs text-fg-muted">{matched.key}</span>
            <span className="text-sm text-fg-2">{matched.label}</span>
          </div>
        ) : (
          <div className="text-sm text-fg-2">
            <Markdown content={resolution.answer || "(empty)"} />
          </div>
        )}
      </div>
    );
  }

  if (resolution.kind === "timeout") {
    return (
      <div className="rounded-lg bg-amber/5 p-4 outline-1 -outline-offset-1 outline-amber/20">
        <div className="flex items-center gap-2 text-xs">
          <ExclamationTriangleIcon className="size-3.5 text-amber" aria-hidden="true" />
          <span className="font-medium uppercase tracking-wider text-amber">Timed out</span>
          <span className="ml-auto inline-flex items-center gap-1 font-mono tabular-nums text-fg-muted">
            <ClockIcon className="size-3" aria-hidden="true" />
            {formatDurationMs(resolution.durationMs)}
          </span>
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-lg bg-coral/5 p-4 outline-1 -outline-offset-1 outline-coral/20">
      <div className="flex items-center gap-2 text-xs">
        <NoSymbolIcon className="size-3.5 text-coral" aria-hidden="true" />
        <span className="font-medium uppercase tracking-wider text-coral">Interrupted</span>
        {resolution.actor && (
          <span className="text-fg-muted">
            by <span className="font-mono text-fg-3">{resolution.actor}</span>
          </span>
        )}
        <span className="ml-auto inline-flex items-center gap-1 font-mono tabular-nums text-fg-muted">
          <ClockIcon className="size-3" aria-hidden="true" />
          {formatDurationMs(resolution.durationMs)}
        </span>
      </div>
      {resolution.reason && (
        <p className="mt-2 text-sm text-fg-3">Reason: {resolution.reason}</p>
      )}
    </div>
  );
}

function PendingBlock({ stageActive }: { stageActive: boolean }) {
  return (
    <div className="rounded-lg bg-overlay-strong p-4 outline-1 -outline-offset-1 outline-line-strong">
      <div className="flex items-center gap-2 text-xs">
        {stageActive ? (
          <ArrowPathIcon
            className="size-3.5 animate-spin text-teal-500"
            aria-hidden="true"
          />
        ) : (
          <ClockIcon className="size-3.5 text-fg-muted" aria-hidden="true" />
        )}
        <span className="font-medium uppercase tracking-wider text-fg-muted">
          {stageActive ? "Awaiting answer…" : "Unanswered"}
        </span>
      </div>
    </div>
  );
}

function QuestionBlock({
  pair,
  stageActive,
}: {
  pair: HumanInterviewPair;
  stageActive: boolean;
}) {
  const { question, resolution } = pair;
  return (
    <article className="space-y-3">
      <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="inline-flex items-center gap-1.5 rounded-full bg-overlay-strong px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider text-fg-2">
          <ChatBubbleLeftEllipsisIcon className="size-3" aria-hidden="true" />
          {questionTypeLabel(question.questionType)}
        </span>
        {question.timeoutSeconds && (
          <span className="text-xs text-fg-muted">
            timeout {Math.round(question.timeoutSeconds)}s
          </span>
        )}
        <Tooltip label={formatAbsoluteTs(question.ts)}>
          <span className="ml-auto font-mono text-[11px] tabular-nums text-fg-muted">
            {new Date(question.ts).toLocaleTimeString()}
          </span>
        </Tooltip>
      </header>

      <div className="rounded-lg bg-panel p-4 outline-1 -outline-offset-1 outline-line">
        <Markdown content={question.question} />
        {question.contextDisplay && (
          <div className="mt-3 border-t border-line pt-3 text-xs text-fg-muted">
            <Markdown content={question.contextDisplay} />
          </div>
        )}
        {question.options.length > 0 && (
          <ul className="mt-3 space-y-1.5">
            {question.options.map((option) => (
              <li
                key={option.key}
                className="flex items-baseline gap-3 rounded-md bg-overlay px-3 py-1.5"
              >
                <span className="inline-flex size-5 shrink-0 items-center justify-center rounded bg-overlay-strong font-mono text-[11px] text-fg-2">
                  {option.key}
                </span>
                <span className="min-w-0 text-sm text-fg-3">
                  <span>{option.label}</span>
                  {option.description && (
                    <span className="mt-0.5 block text-xs/5 text-fg-muted">
                      {option.description}
                    </span>
                  )}
                </span>
              </li>
            ))}
            {question.allowFreeform && (
              <li className="text-xs italic text-fg-muted">Freeform answer also accepted</li>
            )}
          </ul>
        )}
      </div>

      {resolution ? (
        <ResolutionBlock resolution={resolution} options={question.options} />
      ) : (
        <PendingBlock stageActive={stageActive} />
      )}
    </article>
  );
}

export function HumanQA({
  stage,
  events,
}: {
  stage: Stage;
  events: EventEnvelope[];
}) {
  const pairs = useMemo(() => parseHumanInterviewPairs(events), [events]);
  const stageActive = ACTIVE_STAGE_STATES.has(stage.status);
  const pendingCount = pairs.filter((p) => p.resolution == null).length;

  return (
    <div className="space-y-6 pl-3 pr-4 sm:pr-6 lg:pr-8">
      <StageMetaBar stage={stage}>
        <span className="inline-flex items-center gap-1 text-xs text-fg-muted">
          <span className="font-mono tabular-nums">{pairs.length}</span>
          {pairs.length === 1 ? "question" : "questions"}
          {pendingCount > 0 && (
            <span className="ml-2 inline-flex items-center rounded-full bg-amber/15 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wider text-amber">
              {pendingCount} pending
            </span>
          )}
        </span>
      </StageMetaBar>

      {pairs.length === 0 ? (
        <p className="text-sm text-fg-muted">
          {stageActive ? "Waiting for the first question…" : "No questions were asked."}
        </p>
      ) : (
        <ol className="space-y-6">
          {pairs.map((pair) => (
            <li key={pair.question.questionId}>
              <QuestionBlock pair={pair} stageActive={stageActive} />
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
