import { useCallback, useState } from "react";
import {
  ArrowRightIcon,
  CheckIcon,
  ChevronRightIcon,
} from "@heroicons/react/20/solid";
import { QuestionType } from "@qltysh/fabro-api-client";
import type {
  ApiQuestion,
  InterviewOption,
} from "@qltysh/fabro-api-client";

import {
  useSubmitInterviewAnswer,
  type SubmitInterviewAnswerArg,
} from "../lib/mutations";
import { ApiError } from "../lib/api-client";
import { displayLabel } from "./interview-label";
import {
  ReviewTargetQuestion,
  safeReviewTarget,
} from "./review-target-question";
import {
  DockComposer,
  RunDockShell,
  DOCK_CHOICE_BUTTON,
  DOCK_CHOICE_BUTTON_SELECTED,
  DOCK_HEADER_BUTTON,
} from "./run-dock";
import { Spinner } from "./state";
import {
  ErrorMessage,
  PRIMARY_BUTTON_CLASS,
} from "./ui";

/**
 * Options stack into a list once a label is long enough that a row of pills
 * would wrap mid-sentence.
 */
const STACK_LABEL_LENGTH = 40;

/** Shared by the plain question text and the review target rendering. */
const QUESTION_TEXT = "max-w-[78ch] text-base/6 font-medium text-pretty text-fg";

type SubmitInterviewAnswer = SubmitInterviewAnswerArg["answer"];

export interface InterviewDockProps {
  runId: string;
  questions: ApiQuestion[];
}

export function InterviewDock({ runId, questions }: InterviewDockProps) {
  const [activeIndex, setActiveIndex] = useState(0);

  const safeIndex = activeIndex < questions.length ? activeIndex : 0;
  const question = questions[safeIndex];

  if (!question) return null;

  const moreCount = questions.length - 1;

  return (
    // Keyed by question id, so a new question always arrives expanded with an
    // empty composer. A collapsed panel can never silently block a run.
    <InterviewQuestionDock
      key={question.id}
      runId={runId}
      question={question}
      moreCount={moreCount}
      onCycle={() =>
        setActiveIndex((index) => (index + 1) % questions.length)
      }
    />
  );
}

function InterviewQuestionDock({
  runId,
  question,
  moreCount,
  onCycle,
}: {
  runId: string;
  question: ApiQuestion;
  moreCount: number;
  onCycle: () => void;
}) {
  const submitMutation = useSubmitInterviewAnswer(runId);
  const [error, setError] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState(false);
  const submitting = submitMutation.isMutating;
  const reviewTarget = safeReviewTarget(question.review_target);

  const submit = useCallback(
    async (answer: SubmitInterviewAnswer) => {
      setError(null);
      try {
        await submitMutation.trigger({ questionId: question.id, answer });
        return true;
      } catch (caught) {
        setError(interviewSubmitErrorMessage(caught));
        return false;
      }
    },
    [question.id, submitMutation],
  );

  return (
    <RunDockShell
      label="Interview question"
      tone="waiting"
      status="Awaiting input"
      stage={question.stage}
      peek={question.text}
      collapsed={collapsed}
      onCollapsedChange={setCollapsed}
      headerActions={
        moreCount > 0 && (
          <button
            type="button"
            onClick={onCycle}
            className={DOCK_HEADER_BUTTON}
          >
            <span className="tabular-nums">{moreCount}</span> more pending
            <ArrowRightIcon className="size-3" aria-hidden="true" />
          </button>
        )
      }
      body={
        <>
          {reviewTarget ? (
            <ReviewTargetQuestion
              target={reviewTarget}
              className={QUESTION_TEXT}
            />
          ) : (
            <p className={QUESTION_TEXT}>{question.text}</p>
          )}
          {question.context_display && (
            <ContextPanel text={question.context_display} />
          )}
        </>
      }
      actions={
        <>
          <QuestionBody
            question={question}
            submitting={submitting}
            onSubmit={submit}
          />
          {error && <ErrorMessage message={error} />}
        </>
      }
    />
  );
}

/**
 * Context arrives collapsed. It repeats material the operator has usually
 * already read in the stage stream above, so it earns a line rather than a
 * standing panel.
 */
function ContextPanel({ text }: { text: string }) {
  return (
    <details className="group rounded-lg bg-panel-alt outline-1 -outline-offset-1 outline-line">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 rounded-lg px-3 py-1.5 font-mono text-[0.6875rem] tracking-wide text-fg-muted uppercase transition-colors hover:text-fg-3 focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500 [&::-webkit-details-marker]:hidden">
        <ChevronRightIcon
          className="size-3 shrink-0 transition-transform group-open:rotate-90"
          aria-hidden="true"
        />
        Context from preceding stage
        <span className="ml-auto truncate pl-3 font-sans text-xs tracking-normal normal-case group-open:hidden">
          {contextPreview(text)}
        </span>
      </summary>
      <div className="px-3 pb-2.5 text-sm/6 text-fg-2">
        <pre className="font-sans whitespace-pre-wrap">{text}</pre>
      </div>
    </details>
  );
}

/** First line of the context, for the collapsed summary. */
export function contextPreview(text: string): string {
  let lineStart = 0;
  while (lineStart < text.length) {
    const newline = text.indexOf("\n", lineStart);
    const lineEnd = newline === -1 ? text.length : newline;
    const line = text.slice(lineStart, lineEnd).trim();
    if (line) {
      return line.length > 60 ? `${line.slice(0, 60).trimEnd()}…` : line;
    }
    if (newline === -1) break;
    lineStart = newline + 1;
  }
  return "";
}

function QuestionBody({
  question,
  submitting,
  onSubmit,
}: {
  question: ApiQuestion;
  submitting: boolean;
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<boolean>;
}) {
  switch (question.question_type) {
    case QuestionType.YES_NO:
      return <YesNoBody submitting={submitting} onSubmit={onSubmit} />;
    case QuestionType.CONFIRMATION:
      return <ConfirmationBody submitting={submitting} onSubmit={onSubmit} />;
    case QuestionType.MULTI_SELECT:
      return (
        <MultiSelectBody
          options={question.options}
          submitting={submitting}
          onSubmit={onSubmit}
        />
      );
    case QuestionType.MULTIPLE_CHOICE:
      return (
        <ChoiceBody
          options={question.options}
          allowFreeform={question.allow_freeform}
          submitting={submitting}
          onSubmit={onSubmit}
        />
      );
    case QuestionType.FREEFORM:
      return (
        <FreeformAnswer
          submitting={submitting}
          onSubmit={onSubmit}
          placeholder="Write your response…"
        />
      );
    default:
      return null;
  }
}

function YesNoBody({
  submitting,
  onSubmit,
}: {
  submitting: boolean;
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<boolean>;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      {/* react-doctor-disable-next-line react-doctor/design-no-vague-button-label -- Yes/no interview answers conventionally use the literal answer as the visible button label. */}
      <button
        type="button"
        aria-label="Answer no"
        disabled={submitting}
        onClick={() => void onSubmit({ kind: "no" })}
        className={DOCK_CHOICE_BUTTON}
      >
        No
      </button>
      <button
        type="button"
        aria-label="Answer yes"
        disabled={submitting}
        onClick={() => void onSubmit({ kind: "yes" })}
        className={PRIMARY_BUTTON_CLASS}
      >
        {submitting ? (
          <Spinner className="size-4" />
        ) : (
          <CheckIcon className="size-4" aria-hidden="true" />
        )}
        Yes
      </button>
    </div>
  );
}

function ConfirmationBody({
  submitting,
  onSubmit,
}: {
  submitting: boolean;
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<boolean>;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <button
        type="button"
        disabled={submitting}
        onClick={() => void onSubmit({ kind: "yes" })}
        className={PRIMARY_BUTTON_CLASS}
      >
        {submitting ? (
          <Spinner className="size-4" />
        ) : (
          <CheckIcon className="size-4" aria-hidden="true" />
        )}
        Confirm
      </button>
    </div>
  );
}

/**
 * Long labels wrap badly as pills, so they become a stacked list instead.
 */
export function shouldStackOptions(options: InterviewOption[]): boolean {
  return options.some(
    (option) =>
      option.label.length > STACK_LABEL_LENGTH ||
      Boolean(option.description) ||
      Boolean(option.preview),
  );
}

function optionListClass(stacked: boolean): string {
  return stacked
    ? "flex flex-col items-stretch gap-2"
    : "flex flex-wrap items-center gap-2";
}

function ChoiceBody({
  options,
  allowFreeform,
  submitting,
  onSubmit,
}: {
  options: InterviewOption[];
  allowFreeform: boolean;
  submitting: boolean;
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<boolean>;
}) {
  const stacked = shouldStackOptions(options);

  return (
    <div className="space-y-2.5">
      {options.length > 0 && (
        <div className={optionListClass(stacked)}>
          {options.map((option) => (
            <button
              key={option.key}
              type="button"
              disabled={submitting}
              onClick={() => void onSubmit({ kind: "selected", option_key: option.key })}
              className={
                stacked ? `${DOCK_CHOICE_BUTTON} justify-start` : DOCK_CHOICE_BUTTON
              }
            >
              <OptionLabel option={option} />
            </button>
          ))}
        </div>
      )}
      {allowFreeform && (
        <FreeformAnswer
          submitting={submitting}
          onSubmit={onSubmit}
          placeholder={
            options.length > 0
              ? "Or write a custom response…"
              : "Write your response…"
          }
        />
      )}
    </div>
  );
}

function MultiSelectBody({
  options,
  submitting,
  onSubmit,
}: {
  options: InterviewOption[];
  submitting: boolean;
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<boolean>;
}) {
  const [selected, setSelected] = useState<Set<string>>(new Set());

  function toggle(key: string) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  const selectedKeys: string[] = [];
  for (const option of options) {
    if (selected.has(option.key)) selectedKeys.push(option.key);
  }

  const stacked = shouldStackOptions(options);

  return (
    <div className="space-y-2.5">
      <div className={optionListClass(stacked)}>
        {options.map((option) => {
          const isSelected = selected.has(option.key);
          const base = isSelected ? DOCK_CHOICE_BUTTON_SELECTED : DOCK_CHOICE_BUTTON;
          return (
            <button
              key={option.key}
              type="button"
              disabled={submitting}
              aria-pressed={isSelected}
              onClick={() => toggle(option.key)}
              className={stacked ? `${base} justify-start` : base}
            >
              {isSelected && <CheckIcon className="size-3.5" aria-hidden="true" />}
              <OptionLabel option={option} />
            </button>
          );
        })}
      </div>
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs text-fg-muted tabular-nums">
          {selectedKeys.length} selected
        </p>
        <button
          type="button"
          disabled={submitting || selectedKeys.length === 0}
          onClick={() => void onSubmit({ kind: "multi_selected", option_keys: selectedKeys })}
          className={PRIMARY_BUTTON_CLASS}
        >
          {submitting ? (
            <Spinner className="size-4" />
          ) : (
            <CheckIcon className="size-4" aria-hidden="true" />
          )}
          Submit selection
        </button>
      </div>
    </div>
  );
}

function FreeformAnswer({
  submitting,
  onSubmit,
  placeholder,
}: {
  submitting: boolean;
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<boolean>;
  placeholder: string;
}) {
  return (
    <DockComposer
      onSubmit={(text) => onSubmit({ kind: "text", text })}
      placeholder={placeholder}
      submitLabel="Send"
      submitting={submitting}
      ariaLabel="Interview answer"
    />
  );
}

function OptionLabel({ option }: { option: InterviewOption }) {
  return (
    <span className="text-left">
      <span className="block">{displayLabel(option.label)}</span>
      {option.description && (
        <span className="mt-0.5 block text-xs/5 font-normal text-fg-muted">
          {option.description}
        </span>
      )}
      {option.preview && (
        <span
          data-testid="interview-option-preview"
          className="mt-1 block whitespace-pre-wrap rounded bg-overlay-strong px-2 py-1 font-mono text-[11px]/4 font-normal text-fg-3"
        >
          {option.preview}
        </span>
      )}
    </span>
  );
}

function interviewSubmitErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    return error.requestId
      ? `${error.message} Request ID: ${error.requestId}`
      : error.message;
  }
  return error instanceof Error ? error.message : "Couldn't submit your answer.";
}
