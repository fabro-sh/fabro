import {
  useCallback,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import {
  ArrowPathIcon,
  ArrowRightIcon,
  ArrowUturnLeftIcon,
  CheckIcon,
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
import { ErrorMessage } from "./ui";

const PRIMARY_BUTTON =
  "inline-flex items-center justify-center gap-1.5 rounded-lg bg-teal-500 px-3.5 py-2 text-sm font-medium text-on-primary transition-colors hover:bg-teal-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-teal-500";

const CHOICE_BUTTON =
  "inline-flex items-center justify-center gap-1.5 rounded-lg bg-overlay px-3.5 py-2 text-sm font-medium text-fg-2 outline-1 -outline-offset-1 outline-line-strong transition-colors hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60";

const CHOICE_BUTTON_SELECTED =
  "inline-flex items-center justify-center gap-1.5 rounded-lg bg-teal-500/15 px-3.5 py-2 text-sm font-medium text-fg outline-1 -outline-offset-1 outline-teal-500/60 transition-colors hover:bg-teal-500/20 focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500";

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
  const submitting = submitMutation.isMutating;

  const submit = useCallback(
    async (answer: SubmitInterviewAnswer) => {
      setError(null);
      try {
        await submitMutation.trigger({ questionId: question.id, answer });
      } catch (caught) {
        setError(interviewSubmitErrorMessage(caught));
      }
    },
    [question.id, submitMutation],
  );

  return (
    <section aria-label="Interview question">
      <DockHeader
        stage={question.stage}
        moreCount={moreCount}
        onCycle={onCycle}
      />
      <div className="space-y-5 px-5 py-4 sm:px-6">
        <div>
          <p className="text-pretty text-base/6 font-medium text-fg">
            {question.text}
          </p>
          <p className="mt-1 text-xs/5 text-fg-muted">
            {questionTypeLabel(question.question_type)}
          </p>
        </div>

        {question.context_display && (
          <ContextPanel text={question.context_display} />
        )}

        <QuestionBody
          question={question}
          submitting={submitting}
          onSubmit={submit}
        />

        {error && <ErrorMessage message={error} />}
      </div>
    </section>
  );
}

function DockHeader({
  stage,
  moreCount,
  onCycle,
}: {
  stage: string;
  moreCount: number;
  onCycle: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 border-b border-line px-5 py-2.5">
      <div className="flex min-w-0 items-center gap-2 text-sm">
        <PulseDot />
        <span className="font-medium text-fg-2">Awaiting input</span>
        {stage && (
          <>
            <span className="text-fg-muted" aria-hidden="true">
              ·
            </span>
            <span className="truncate font-mono text-xs text-fg-3">{stage}</span>
          </>
        )}
      </div>
      {moreCount > 0 && (
        <button
          type="button"
          onClick={onCycle}
          className="inline-flex shrink-0 items-center gap-1 rounded-md bg-overlay px-2 py-1 text-xs font-medium text-fg-2 outline-1 -outline-offset-1 outline-line-strong hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
        >
          <span className="tabular-nums">{moreCount}</span> more pending
          <ArrowRightIcon className="size-3" aria-hidden="true" />
        </button>
      )}
    </div>
  );
}

function PulseDot() {
  return (
    <span className="relative flex size-2 items-center justify-center" aria-hidden="true">
      <span className="absolute inline-flex size-full animate-ping rounded-full bg-amber/60" />
      <span className="relative inline-flex size-2 rounded-full bg-amber" />
    </span>
  );
}

function ContextPanel({ text }: { text: string }) {
  return (
    <div className="rounded-lg bg-panel-alt p-4 outline-1 -outline-offset-1 outline-line">
      <p className="mb-1.5 font-mono text-[0.6875rem] tracking-wide text-fg-muted uppercase">
        Context from preceding stage
      </p>
      <div className="max-h-40 overflow-y-auto text-sm/6 text-fg-2">
        <pre className="font-sans whitespace-pre-wrap">{text}</pre>
      </div>
    </div>
  );
}

function QuestionBody({
  question,
  submitting,
  onSubmit,
}: {
  question: ApiQuestion;
  submitting: boolean;
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<void>;
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
        <FreeformBody
          submitting={submitting}
          onSubmit={onSubmit}
          placeholder="Write your response…"
          submitLabel="Send"
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
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<void>;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      {/* react-doctor-disable-next-line react-doctor/design-no-vague-button-label -- Yes/no interview answers conventionally use the literal answer as the visible button label. */}
      <button
        type="button"
        aria-label="Answer no"
        disabled={submitting}
        onClick={() => void onSubmit({ kind: "no" })}
        className={CHOICE_BUTTON}
      >
        No
      </button>
      <button
        type="button"
        aria-label="Answer yes"
        disabled={submitting}
        onClick={() => void onSubmit({ kind: "yes" })}
        className={PRIMARY_BUTTON}
      >
        {submitting ? <Spinner /> : <CheckIcon className="size-4" aria-hidden="true" />}
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
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<void>;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <button
        type="button"
        disabled={submitting}
        onClick={() => void onSubmit({ kind: "yes" })}
        className={PRIMARY_BUTTON}
      >
        {submitting ? <Spinner /> : <CheckIcon className="size-4" aria-hidden="true" />}
        Confirm
      </button>
    </div>
  );
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
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<void>;
}) {
  return (
    <div className="space-y-4">
      {options.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          {options.map((option) => (
            <button
              key={option.key}
              type="button"
              disabled={submitting}
              onClick={() => void onSubmit({ kind: "selected", option_key: option.key })}
              className={CHOICE_BUTTON}
            >
              <OptionLabel option={option} />
            </button>
          ))}
        </div>
      )}
      {allowFreeform && (
        <FreeformBody
          submitting={submitting}
          onSubmit={onSubmit}
          placeholder={
            options.length > 0
              ? "Or write a custom response…"
              : "Write your response…"
          }
          submitLabel="Send"
          divider={options.length > 0}
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
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<void>;
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

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        {options.map((option) => {
          const isSelected = selected.has(option.key);
          return (
            <button
              key={option.key}
              type="button"
              disabled={submitting}
              aria-pressed={isSelected}
              onClick={() => toggle(option.key)}
              className={isSelected ? CHOICE_BUTTON_SELECTED : CHOICE_BUTTON}
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
          className={PRIMARY_BUTTON}
        >
          {submitting ? <Spinner /> : <CheckIcon className="size-4" aria-hidden="true" />}
          Submit selection
        </button>
      </div>
    </div>
  );
}

function FreeformBody({
  submitting,
  onSubmit,
  placeholder,
  submitLabel,
  divider = false,
}: {
  submitting: boolean;
  onSubmit: (answer: SubmitInterviewAnswer) => Promise<void>;
  placeholder: string;
  submitLabel: string;
  divider?: boolean;
}) {
  const [value, setValue] = useState("");

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmed = value.trim();
    if (!trimmed || submitting) return;
    await onSubmit({ kind: "text", text: trimmed });
    setValue("");
  }

  function handleKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      const form = event.currentTarget.form;
      if (form) form.requestSubmit();
    }
  }

  const disabled = submitting || value.trim().length === 0;

  return (
    <form onSubmit={handleSubmit} className="space-y-2">
      {divider && (
        <div className="flex items-center gap-3" aria-hidden="true">
          <span className="h-px flex-1 bg-line" />
          <span className="text-xs text-fg-muted">or</span>
          <span className="h-px flex-1 bg-line" />
        </div>
      )}
      <div className="flex items-end gap-2">
        <label className="sr-only" htmlFor="interview-freeform-answer">
          Your response
        </label>
        <textarea
          id="interview-freeform-answer"
          name="answer"
          aria-label="Interview answer"
          rows={1}
          value={value}
          onChange={(event) => setValue(event.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          disabled={submitting}
          className="block w-full resize-none rounded-lg bg-panel-alt px-3.5 py-2.5 text-base/6 text-fg outline-1 -outline-offset-1 outline-line-strong placeholder:text-fg-muted focus:outline-2 focus:-outline-offset-1 focus:outline-teal-500 disabled:opacity-60 sm:text-sm/5"
        />
        <button type="submit" disabled={disabled} className={PRIMARY_BUTTON}>
          {submitting ? (
            <Spinner />
          ) : (
            <ArrowUturnLeftIcon
              className="size-3.5 -scale-x-100"
              aria-hidden="true"
            />
          )}
          {submitLabel}
        </button>
      </div>
      <p className="text-xs text-fg-muted">
        Press <kbd className="rounded bg-overlay px-1 font-mono text-[0.6875rem]">Enter</kbd> to
        send · <kbd className="rounded bg-overlay px-1 font-mono text-[0.6875rem]">Shift</kbd>+
        <kbd className="rounded bg-overlay px-1 font-mono text-[0.6875rem]">Enter</kbd> for a new line
      </p>
    </form>
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
    </span>
  );
}

function Spinner() {
  return <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />;
}

function questionTypeLabel(type: QuestionType): string {
  switch (type) {
    case QuestionType.YES_NO:
      return "Yes or no";
    case QuestionType.CONFIRMATION:
      return "Confirmation required";
    case QuestionType.MULTIPLE_CHOICE:
      return "Pick one";
    case QuestionType.MULTI_SELECT:
      return "Pick one or more";
    case QuestionType.FREEFORM:
      return "Freeform response";
    default:
      return "";
  }
}

function interviewSubmitErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    return error.requestId
      ? `${error.message} Request ID: ${error.requestId}`
      : error.message;
  }
  return error instanceof Error ? error.message : "Couldn't submit your answer.";
}
