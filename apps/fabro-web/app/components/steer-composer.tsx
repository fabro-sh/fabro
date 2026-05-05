import { useEffect, useRef, useState } from "react";

import { ApiError } from "../lib/api-client";
import { useSteerRun } from "../lib/mutations";
import { ErrorMessage } from "./ui";

interface SteerComposerProps {
  runId: string;
  open: boolean;
  onClose: () => void;
}

/**
 * Modal composer for sending a mid-run steering message to a running run.
 *
 * Renders a textarea + two action buttons:
 * - Send: appends to the steering queue (default).
 * - Interrupt: cancels the in-flight LLM stream / tool calls and delivers
 *   the message as the next user turn.
 *
 * Surfaces 409 errors inline so users see the rejection reason (e.g., the
 * `cli_agent_not_steerable` code).
 */
export function SteerComposer({ runId, open, onClose }: SteerComposerProps) {
  const [text, setText] = useState("");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const { trigger, isMutating } = useSteerRun(runId);

  // Autofocus when opening; reset state when closing.
  useEffect(() => {
    if (open) {
      requestAnimationFrame(() => textareaRef.current?.focus());
    } else {
      setText("");
      setErrorMessage(null);
    }
  }, [open]);

  // Close on Escape.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const trimmed = text.trim();
  const canSubmit = trimmed.length > 0 && !isMutating;

  async function send(interrupt: boolean) {
    if (!canSubmit) return;
    setErrorMessage(null);
    try {
      await trigger({ text: trimmed, interrupt });
      onClose();
    } catch (err) {
      if (err instanceof ApiError) {
        // Try to surface the well-known 409 codes inline.
        const body = err.body as { code?: string; detail?: string } | null;
        if (body?.code === "cli_agent_not_steerable") {
          setErrorMessage(
            "All running agent stages are CLI-mode and can't be steered.",
          );
        } else if (body?.code === "use_answer_endpoint") {
          setErrorMessage(
            "Run is blocked on a question; answer the question first.",
          );
        } else {
          setErrorMessage(body?.detail ?? err.message ?? "Steer failed.");
        }
      } else {
        setErrorMessage("Steer failed; try again.");
      }
    }
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send(false);
    }
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Steer running agent"
        className="w-full max-w-md rounded-lg border border-line bg-bg-elevated p-4 shadow-lg"
      >
        <div className="mb-2 text-sm font-semibold text-fg">Steer agent</div>
        <textarea
          ref={textareaRef}
          rows={4}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Type a steering message…"
          className="w-full resize-none rounded-md border border-line bg-bg p-2 text-sm text-fg outline-none focus:border-teal-500/50"
          maxLength={8192}
        />
        {errorMessage && (
          <div className="mt-2">
            <ErrorMessage message={errorMessage} />
          </div>
        )}
        <div className="mt-3 flex items-center justify-between gap-2">
          <span className="text-[11px] text-fg-muted">
            Enter to send · Shift+Enter for newline
          </span>
          <div className="flex gap-2">
            <button
              type="button"
              onClick={onClose}
              className="rounded-md border border-line px-3 py-1 text-xs text-fg-2 hover:border-line-strong"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={() => void send(true)}
              disabled={!canSubmit}
              className="rounded-md border border-amber/30 px-3 py-1 text-xs text-amber hover:border-amber/60 disabled:cursor-not-allowed disabled:opacity-50"
            >
              Interrupt
            </button>
            <button
              type="button"
              onClick={() => void send(false)}
              disabled={!canSubmit}
              className="rounded-md bg-teal-500 px-3 py-1 text-xs font-medium text-white hover:bg-teal-600 disabled:cursor-not-allowed disabled:opacity-50"
            >
              Send
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}