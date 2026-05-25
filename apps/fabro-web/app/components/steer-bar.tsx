import {
  useImperativeHandle,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type Ref,
} from "react";

import { ApiError } from "../lib/api-client";
import { useInterruptRun, useSteerRun } from "../lib/mutations";
import { ErrorMessage } from "./ui";

export interface SteerBarProps {
  runId: string;
  ref?: Ref<SteerBarHandle>;
}

export interface SteerBarHandle {
  focus(): void;
}

export function SteerBar({ runId, ref }: SteerBarProps) {
  const [text, setText] = useState("");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const steer = useSteerRun(runId);
  const interrupt = useInterruptRun(runId);
  const pending = steer.isMutating || interrupt.isMutating;

  useImperativeHandle(ref, () => ({
    focus() {
      textareaRef.current?.focus();
    },
  }));

  const trimmed = text.trim();
  const canSend = trimmed.length > 0 && !pending;

  async function sendSteering() {
    if (!canSend) return;
    setErrorMessage(null);
    try {
      await steer.trigger({ text: trimmed, interrupt: false });
      setText("");
    } catch (err) {
      setErrorMessage(formatSteerError(err));
    }
  }

  async function fireInterrupt() {
    if (pending) return;
    setErrorMessage(null);
    try {
      await interrupt.trigger();
    } catch (err) {
      setErrorMessage(formatInterruptError(err));
    }
  }

  function handleSubmit(e: FormEvent) {
    e.preventDefault();
    void sendSteering();
  }

  function handleKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void sendSteering();
    }
  }

  return (
    <form
      onSubmit={handleSubmit}
      aria-label="Steer running agent"
      className="mx-auto max-w-4xl px-4 py-3 sm:px-6 lg:px-8"
    >
      <div className="flex items-end gap-2">
        <textarea
          ref={textareaRef}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Steer the agent…"
          rows={1}
          maxLength={8192}
          aria-label="Steering message"
          className="flex-1 resize-none rounded-md bg-overlay px-3 py-2 text-sm text-fg outline-1 -outline-offset-1 outline-line-strong placeholder:text-fg-muted focus:outline-2 focus:-outline-offset-1 focus:outline-teal-500"
        />
        <button
          type="button"
          onClick={() => void fireInterrupt()}
          disabled={pending}
          className="inline-flex shrink-0 items-center gap-2 rounded-md bg-overlay px-3 py-2 text-sm font-medium text-amber outline-1 -outline-offset-1 outline-amber/40 transition-colors hover:bg-amber/15 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-amber disabled:cursor-not-allowed disabled:opacity-60"
        >
          {interrupt.isMutating ? "Interrupting…" : "Interrupt"}
        </button>
        <button
          type="submit"
          disabled={!canSend}
          className="inline-flex shrink-0 items-center justify-center rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-on-primary transition-colors hover:bg-teal-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-teal-500"
        >
          {steer.isMutating ? "Sending…" : "Send"}
        </button>
      </div>
      {errorMessage && (
        <div className="mt-2">
          <ErrorMessage message={errorMessage} />
        </div>
      )}
    </form>
  );
}

function formatSteerError(err: unknown): string {
  if (err instanceof ApiError) {
    const body = err.body as { code?: string; detail?: string } | null;
    if (body?.code === "use_answer_endpoint") {
      return "Run is blocked on a question; answer the question first.";
    }
    return body?.detail ?? err.message ?? "Steer failed.";
  }
  return "Steer failed; try again.";
}

function formatInterruptError(err: unknown): string {
  if (err instanceof ApiError) {
    const body = err.body as { detail?: string } | null;
    return body?.detail ?? err.message ?? "Interrupt failed.";
  }
  return "Interrupt failed; try again.";
}
