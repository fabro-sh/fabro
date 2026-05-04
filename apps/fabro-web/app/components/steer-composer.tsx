import { useState, useCallback, useRef, useEffect } from "react";

import { useSteerRun } from "../lib/mutations";

interface SteerComposerProps {
  runId: string;
  onClose: () => void;
}

export function SteerComposer({ runId, onClose }: SteerComposerProps) {
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const steerMutation = useSteerRun(runId);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  const submit = useCallback(
    async (interrupt: boolean) => {
      if (!text.trim()) return;
      setError(null);
      try {
        await steerMutation.trigger({ text: text.trim(), interrupt });
        setText("");
        onClose();
      } catch (err: unknown) {
        const message =
          err instanceof Error ? err.message : "Failed to deliver steer.";
        setError(message);
      }
    },
    [text, steerMutation, onClose],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        submit(false);
      }
    },
    [submit],
  );

  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-0 z-30">
      <div className="bg-linear-to-t from-page via-page/80 to-transparent pt-10">
        <div className="pointer-events-auto mx-auto max-w-3xl px-4 pb-4">
          <div className="rounded-2xl bg-panel shadow-lg ring-1 ring-line-strong p-4">
            <div className="mb-3 flex items-center justify-between">
              <h3 className="text-sm font-medium text-fg">Steer agent</h3>
              <button
                onClick={onClose}
                className="text-xs text-fg-muted hover:text-fg"
              >
                Close
              </button>
            </div>
            <textarea
              ref={textareaRef}
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder="Type a steering message…"
              rows={3}
              className="w-full resize-none rounded-lg border border-line bg-page px-3 py-2 text-sm text-fg-2 placeholder:text-fg-muted focus:border-teal-500 focus:outline-none focus:ring-1 focus:ring-teal-500"
            />
            {error && <p className="mt-2 text-xs text-coral">{error}</p>}
            <div className="mt-3 flex justify-end gap-2">
              <button
                onClick={() => submit(true)}
                disabled={!text.trim() || steerMutation.isMutating}
                className="rounded-lg border border-line px-3 py-1.5 text-xs font-medium text-fg-2 hover:bg-overlay disabled:opacity-50"
              >
                Interrupt &amp; Send
              </button>
              <button
                onClick={() => submit(false)}
                disabled={!text.trim() || steerMutation.isMutating}
                className="rounded-lg bg-teal-500 px-3 py-1.5 text-xs font-medium text-on-primary hover:bg-teal-300 disabled:opacity-50"
              >
                Send
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
