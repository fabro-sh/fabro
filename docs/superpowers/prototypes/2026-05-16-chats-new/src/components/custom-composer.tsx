import { ComposerPrimitive, ThreadPrimitive } from "@assistant-ui/react";
import { ArrowUpIcon } from "@heroicons/react/24/solid";
import { StopIcon } from "@heroicons/react/24/outline";

import ComposerChips from "./composer-chips";

export default function CustomComposer() {
  return (
    <ComposerPrimitive.Root className="mx-auto mb-4 flex w-full max-w-2xl flex-col overflow-hidden rounded-2xl bg-panel-alt/80 shadow-lg shadow-black/30 ring-1 ring-line-strong backdrop-blur-sm focus-within:ring-teal-500/40">
      <ComposerPrimitive.Input
        rows={1}
        autoFocus
        placeholder="Ask anything…"
        className="block max-h-48 w-full resize-none bg-transparent px-4 pt-4 pb-2 text-sm text-fg placeholder:text-fg-muted focus:outline-none"
      />
      <div className="flex items-center justify-between gap-3 px-4 pt-2 pb-3">
        <ComposerChips />
        <ThreadPrimitive.If running>
          <ComposerPrimitive.Cancel asChild>
            <button
              type="button"
              aria-label="Stop"
              className="inline-flex size-9 items-center justify-center rounded-full bg-overlay-strong text-fg transition-colors hover:bg-overlay"
            >
              <StopIcon className="size-4" />
            </button>
          </ComposerPrimitive.Cancel>
        </ThreadPrimitive.If>
        <ThreadPrimitive.If running={false}>
          <ComposerPrimitive.Send asChild>
            <button
              type="submit"
              aria-label="Send message"
              className="inline-flex size-9 items-center justify-center rounded-full bg-teal-500 text-on-primary transition-colors hover:bg-teal-300 disabled:cursor-not-allowed disabled:opacity-50"
            >
              <ArrowUpIcon className="size-4" />
            </button>
          </ComposerPrimitive.Send>
        </ThreadPrimitive.If>
      </div>
    </ComposerPrimitive.Root>
  );
}
