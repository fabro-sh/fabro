import { useMemo, useRef, useState } from "react";
import {
  AssistantRuntimeProvider,
  useLocalRuntime,
} from "@assistant-ui/react";
import { Thread, makeMarkdownText } from "@assistant-ui/react-ui";
import { XMarkIcon } from "@heroicons/react/24/outline";
import remarkGfm from "remark-gfm";

import { createAskFabroAdapter } from "../../lib/ask-fabro-runtime";
import { useAskFabroLayout } from "../../lib/ask-fabro-layout";
import SidebarComposer from "./sidebar-composer";
import SidebarWelcome from "./sidebar-welcome";
import ToolCallSummary from "./tool-call-summary";

// remark-gfm enables GitHub-flavored Markdown so the agent's tables, task
// lists, and strikethrough render instead of leaking as raw `|` syntax.
const MarkdownText = makeMarkdownText({ remarkPlugins: [remarkGfm] });

/** Default (and minimum) width of the docked sidebar in px. */
export const SIDEBAR_WIDTH = 420;

/** The user can drag the sidebar wider, but never past twice its default. */
const SIDEBAR_MAX_WIDTH = SIDEBAR_WIDTH * 2;

/**
 * Right-docked "Ask Fabro" assistant panel. An animated-width column that
 * collapses to zero when closed; renders assistant-ui's `<Thread>` with a
 * stripped composer scoped to the narrow column via the `.ask-fabro-sidebar`
 * CSS in app.css.
 *
 * The sidebar is parameterized by `runId`: the agent's session is scoped to
 * that run (and only that run; the server enforces this via the same-run
 * worker token attached to the session's run-control tools).
 *
 * `width` is owned by the run detail page so it can publish the value to the
 * layout context; the left-edge handle here drags it between `SIDEBAR_WIDTH`
 * and `SIDEBAR_MAX_WIDTH`.
 */
export default function AskFabroSidebar({
  isOpen,
  onClose,
  runId,
  defaultModel,
  width,
  onWidthChange,
}: {
  isOpen: boolean;
  onClose: () => void;
  runId: string;
  defaultModel?: string | null;
  width: number;
  onWidthChange: (width: number) => void;
}) {
  const adapter = useMemo(
    () => createAskFabroAdapter({ runId, defaultModel }),
    [runId, defaultModel],
  );
  const runtime = useLocalRuntime(adapter);

  const { setIsResizing } = useAskFabroLayout();
  const [isDragging, setIsDragging] = useState(false);
  // Pointer X and width captured at drag start, so each move resolves to an
  // absolute width rather than accumulating rounding error.
  const dragOrigin = useRef<{ x: number; width: number } | null>(null);

  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragOrigin.current = { x: event.clientX, width };
    setIsDragging(true);
    setIsResizing(true);
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const origin = dragOrigin.current;
    if (!origin) return;
    // The handle is on the left edge of a right-docked panel: dragging left
    // (clientX decreasing) widens it.
    const next = origin.width + (origin.x - event.clientX);
    onWidthChange(
      Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_WIDTH, next)),
    );
  };

  const endDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragOrigin.current) return;
    event.currentTarget.releasePointerCapture(event.pointerId);
    dragOrigin.current = null;
    setIsDragging(false);
    setIsResizing(false);
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
        {/* react-doctor-disable-next-line react-doctor/prefer-tag-over-role -- This is an interactive, draggable splitter; <hr> would not convey the resize behavior. */}
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
              components={{ Composer: SidebarComposer, ThreadWelcome: SidebarWelcome }}
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
