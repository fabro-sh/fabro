import { useMemo } from "react";
import {
  AssistantRuntimeProvider,
  useLocalRuntime,
} from "@assistant-ui/react";
import { Thread, makeMarkdownText } from "@assistant-ui/react-ui";
import { XMarkIcon } from "@heroicons/react/24/outline";

import { createAskFabroAdapter } from "../../lib/ask-fabro-runtime";
import SidebarComposer from "./sidebar-composer";
import ToolFallback from "./tool-fallback";

const MarkdownText = makeMarkdownText();

export const SIDEBAR_WIDTH = 420;

/**
 * Right-docked "Ask Fabro" assistant panel. An animated-width column that
 * collapses to zero when closed; renders assistant-ui's `<Thread>` with a
 * stripped composer scoped to the narrow column via the `.ask-fabro-sidebar`
 * CSS in app.css.
 *
 * The sidebar is parameterized by `runId`: the agent's session is scoped to
 * that run (and only that run; the server enforces this via the same-run
 * worker token attached to the session's run-control tools).
 */
export default function AskFabroSidebar({
  isOpen,
  onClose,
  runId,
  defaultModel,
}: {
  isOpen: boolean;
  onClose: () => void;
  runId: string;
  defaultModel?: string | null;
}) {
  const adapter = useMemo(
    () => createAskFabroAdapter({ runId, defaultModel }),
    [runId, defaultModel],
  );
  const runtime = useLocalRuntime(adapter);

  return (
    <aside
      aria-label="Ask Fabro"
      aria-hidden={!isOpen}
      style={{ width: isOpen ? SIDEBAR_WIDTH : 0 }}
      className="h-full shrink-0 overflow-hidden transition-[width] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)]"
    >
      <div
        className="fabro-chat ask-fabro-sidebar relative isolate flex h-full flex-col border-l border-line bg-panel/40 backdrop-blur-sm"
        style={{ width: SIDEBAR_WIDTH }}
      >
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
              components={{ Composer: SidebarComposer, ThreadWelcome: () => null }}
              assistantMessage={{
                components: { Text: MarkdownText, ToolFallback },
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
