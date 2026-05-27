import { useMemo, useRef, useState } from "react";
import {
  AssistantRuntimeProvider,
  useLocalRuntime,
} from "@assistant-ui/react";
import { Thread, makeMarkdownText } from "@assistant-ui/react-ui";
import { XMarkIcon } from "@heroicons/react/24/outline";
import remarkGfm from "remark-gfm";

import SidebarComposer from "../../chats/sidebar-composer";
import ToolCallSummary from "../../chats/tool-call-summary";
import type { WorkflowDraft } from "../state/draft";
import type { ToolCall } from "../state/reducer";
import { createPlaygroundAdapter } from "./runtime";
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
 * Tool calls from the LLM stream into `onToolCall`, which the parent wires
 * to the playground draft reducer so the canvas paints live as the model
 * builds the graph.
 */
export default function PlaygroundChatSidebar({
  isOpen,
  onClose,
  chatEndpoint,
  getWorkflow,
  onToolCall,
  width,
  onWidthChange,
}: {
  isOpen: boolean;
  onClose: () => void;
  chatEndpoint: string;
  getWorkflow: () => WorkflowDraft;
  onToolCall: (call: ToolCall) => void;
  width: number;
  onWidthChange: (width: number) => void;
}) {
  // The adapter is referentially-stable across renders because it reads the
  // draft via `getWorkflow` on each turn — no need to memoise on draft.
  const adapter = useMemo(
    () => createPlaygroundAdapter({ chatEndpoint, getWorkflow, onToolCall }),
    [chatEndpoint, getWorkflow, onToolCall],
  );
  const runtime = useLocalRuntime(adapter);

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
                components: { Text: MarkdownText, ToolFallback: ToolCallSummary },
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
