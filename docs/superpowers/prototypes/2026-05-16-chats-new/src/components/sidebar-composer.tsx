import { ComposerPrimitive, ThreadPrimitive } from "@assistant-ui/react";
import { ArrowUpIcon } from "@heroicons/react/24/solid";
import { StopIcon } from "@heroicons/react/24/outline";

/**
 * Compact single-line composer for the Ask-Fabro sidebar. Stripped of the
 * project/branch/model chips; textarea + send button live on one row inside
 * a rounded pill.
 */
export default function SidebarComposer() {
  return (
    <ComposerPrimitive.Root className="m-3 flex w-auto items-end gap-2 rounded-2xl bg-panel-alt/80 px-3 py-2 shadow-md shadow-black/30 ring-1 ring-line-strong backdrop-blur-sm focus-within:ring-teal-500/40">
      <ComposerPrimitive.Input
        rows={1}
        autoFocus
        placeholder="Ask Fabro…"
        className="block max-h-40 min-w-0 flex-1 resize-none bg-transparent py-1 text-sm text-fg placeholder:text-fg-muted focus:outline-none"
      />
      <ThreadPrimitive.If running>
        <ComposerPrimitive.Cancel asChild>
          <button
            type="button"
            aria-label="Stop"
            className="inline-flex size-7 shrink-0 items-center justify-center rounded-full bg-overlay-strong text-fg transition-colors hover:bg-overlay"
          >
            <StopIcon className="size-3.5" />
          </button>
        </ComposerPrimitive.Cancel>
      </ThreadPrimitive.If>
      <ThreadPrimitive.If running={false}>
        <ComposerPrimitive.Send asChild>
          <button
            type="submit"
            aria-label="Send message"
            className="inline-flex size-7 shrink-0 items-center justify-center rounded-full bg-teal-500 text-on-primary transition-colors hover:bg-teal-300 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <ArrowUpIcon className="size-3.5" />
          </button>
        </ComposerPrimitive.Send>
      </ThreadPrimitive.If>
    </ComposerPrimitive.Root>
  );
}
