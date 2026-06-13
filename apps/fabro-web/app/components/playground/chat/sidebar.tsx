import { useCallback, useMemo, useRef, useState } from "react";
import type { AssistantRuntime } from "@assistant-ui/react";
import {
  AssistantRuntimeProvider,
  useLocalRuntime,
} from "@assistant-ui/react";
import { Thread, makeMarkdownText } from "@assistant-ui/react-ui";
import { XMarkIcon } from "@heroicons/react/24/outline";
import remarkGfm from "remark-gfm";

import SidebarComposer from "../../chats/sidebar-composer";
import type { WorkflowDraft } from "../state/draft";
import type { ToolCall } from "../state/reducer";
import { createPlaygroundAdapter } from "./runtime";
import PlaygroundToolCallSummary from "./tool-call-summary";
import PlaygroundWelcome from "./welcome";

const MarkdownText = makeMarkdownText({ remarkPlugins: [remarkGfm] });

const SIDEBAR_WIDTH = 420;
const SIDEBAR_MAX_WIDTH = SIDEBAR_WIDTH * 2;

/**
 * Playground-flavoured Ask Fabro sidebar. Mirrors `AskFabroSidebar`'s
 * look and feel (left-edge drag handle, animated width, stripped composer),
 * but talks to `/api/v1/playground/chat` via `createPlaygroundAdapter`
 * instead of the session-scoped Ask Fabro runtime.
 *
 * Each turn the model emits one `write_workflow_file` tool call with the
 * full new DOT contents; the adapter parses, diffs against the current
 * draft, and animates the resulting reducer ops into the canvas. `dispatch`
 * is called once per animated op; `onParseFailure` fires if the model's
 * DOT couldn't be parsed.
 */
/** Max consecutive auto-retries when the model's DOT fails to parse. */
const MAX_AUTO_RETRIES = 2;

export default function PlaygroundChatSidebar({
  isOpen,
  onClose,
  chatEndpoint,
  getWorkflow,
  dispatch,
  onParseFailure,
  width,
  onWidthChange,
}: {
  isOpen: boolean;
  onClose: () => void;
  chatEndpoint: string;
  getWorkflow: () => WorkflowDraft;
  dispatch: (call: ToolCall) => void;
  onParseFailure?: (info: { message: string; rawContent: string }) => void;
  width: number;
  onWidthChange: (width: number) => void;
}) {
  // The runtime is created from the adapter (chicken-and-egg) so we stash
  // it in a ref after creation. The adapter's onParseFailure callback
  // reads from this ref to call `runtime.thread.append` for auto-retry.
  const runtimeRef = useRef<AssistantRuntime | null>(null);
  const autoRetriesRef = useRef(0);

  const handleParseFailure = useCallback(
    (info: { message: string; rawContent: string }) => {
      if (
        runtimeRef.current !== null &&
        autoRetriesRef.current < MAX_AUTO_RETRIES
      ) {
        autoRetriesRef.current++;
        runtimeRef.current.thread.append({
          role:    "user",
          content: [
            {
              type: "text",
              text:
                `The DOT you wrote couldn't be parsed: ${info.message}. ` +
                `Please re-emit a complete \`workflow.fabro\` with valid syntax.`,
            },
          ],
        });
      }
      onParseFailure?.(info);
    },
    [onParseFailure],
  );

  const handleParseSuccess = useCallback(() => {
    autoRetriesRef.current = 0;
  }, []);

  // The adapter is referentially-stable across renders because it reads the
  // draft via `getWorkflow` on each turn — no need to memoise on draft.
  const adapter = useMemo(
    () =>
      createPlaygroundAdapter({
        chatEndpoint,
        getWorkflow,
        dispatch,
        onParseFailure: handleParseFailure,
        onParseSuccess: handleParseSuccess,
      }),
    [chatEndpoint, getWorkflow, dispatch, handleParseFailure, handleParseSuccess],
  );
  const runtime = useLocalRuntime(adapter);
  runtimeRef.current = runtime;

  const [isDragging, setIsDragging] = useState(false);
  const dragOrigin = useRef<{ x: number; width: number } | null>(null);

  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragOrigin.current = { x: event.clientX, width };
    setIsDragging(true);
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const origin = dragOrigin.current;
    if (!origin) return;
    const next = origin.width + (origin.x - event.clientX);
    onWidthChange(Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_WIDTH, next)));
  };

  const endDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragOrigin.current) return;
    event.currentTarget.releasePointerCapture(event.pointerId);
    dragOrigin.current = null;
    setIsDragging(false);
  };

  return (
    <aside
      aria-label="Ask Fabro"
      aria-hidden={!isOpen}
      style={{ width: isOpen ? width : 0 }}
      className={`h-full shrink-0 overflow-hidden ${
        isDragging
          ? ""
          : "transition-[width] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)]"
      }`}
    >
      <div
        className={`fabro-chat ask-fabro-sidebar relative isolate flex h-full flex-col border-l border-line bg-panel/40 backdrop-blur-sm ${
          isDragging ? "select-none" : ""
        }`}
        style={{ width }}
      >
        {/* react-doctor-disable-next-line react-doctor/prefer-tag-over-role -- Interactive draggable splitter; <hr> wouldn't convey resize. */}
        <div
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize Ask Fabro panel"
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          className="group absolute inset-y-0 left-0 z-20 w-2 cursor-col-resize touch-none"
        >
          <span
            aria-hidden
            className={`absolute inset-y-0 left-0 w-0.5 transition-colors ${
              isDragging
                ? "bg-teal-500"
                : "bg-transparent group-hover:bg-teal-500/60"
            }`}
          />
        </div>
        <header className="flex h-12 shrink-0 items-center justify-end px-2">
          <button
            type="button"
            onClick={onClose}
            aria-label="Close assistant"
            className="inline-flex size-8 items-center justify-center rounded-md text-fg-3 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
          >
            <XMarkIcon className="size-4" />
          </button>
        </header>
        <div className="min-h-0 flex-1">
          <AssistantRuntimeProvider runtime={runtime}>
            <Thread
              components={{
                Composer: SidebarComposer,
                ThreadWelcome: PlaygroundWelcome,
              }}
              assistantMessage={{
                components: { Text: MarkdownText, ToolFallback: PlaygroundToolCallSummary },
                allowCopy: false,
                allowReload: false,
                allowSpeak: false,
                allowFeedbackPositive: false,
                allowFeedbackNegative: false,
              }}
            />
          </AssistantRuntimeProvider>
        </div>
      </div>
    </aside>
  );
}

export { SIDEBAR_WIDTH };
