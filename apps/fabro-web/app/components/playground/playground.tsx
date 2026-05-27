import { useCallback, useRef, useState } from "react";
import { SparklesIcon } from "@heroicons/react/24/solid";

import PlaygroundCanvas from "./canvas/canvas";
import { useSimulation } from "./canvas/use-simulation";
import PlaygroundChatSidebar, {
  SIDEBAR_WIDTH,
} from "./chat/sidebar";
import { usePlaygroundDraft } from "./state/persist";
import type { WorkflowDraft } from "./state/draft";
import type { ToolCall } from "./state/reducer";
import FileTabs from "./ui/file-tabs";
import DownloadButton from "./ui/download-button";
import RunTrace from "./ui/run-trace";
import SimulationControls from "./ui/simulation-controls";

export type PlaygroundAuthMode = "required" | "anonymous";

export type PlaygroundProps = {
  /**
   * URL the chat adapter posts each turn against. Externalised so the same
   * component tree can re-embed in the marketing site against a public,
   * rate-limited variant of the endpoint later.
   */
  chatEndpoint: string;
  /**
   * `required` — assume the parent shell has already enforced authentication
   * (current fabro-web routes do this via `AppShell`).
   * `anonymous` — anonymous embed mode for a future marketing-site island;
   * not used by fabro-web today.
   */
  authMode: PlaygroundAuthMode;
};

/**
 * The playground feature surface, deliberately framed as a standalone
 * component tree so it can later be lifted into the Astro marketing site
 * as a React island (engineering-as-marketing). It must not depend on
 * `AppShell`, react-router context, or any of fabro-web's app-wide stores;
 * any cross-cutting concern (chat endpoint, auth mode, theme) flows in
 * through props.
 *
 * Layout mirrors `/ask-fabro`: workspace on the left, a docked chat
 * column on the right that drives the canvas via streamed tool calls.
 */
export default function Playground({
  chatEndpoint,
  authMode: _authMode,
}: PlaygroundProps) {
  const { draft, applyCall } = usePlaygroundDraft();
  const [isChatOpen, setChatOpen] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(SIDEBAR_WIDTH);
  const sim = useSimulation(draft);

  // The chat adapter is memoized on (chatEndpoint, getWorkflow, onToolCall);
  // we need both callbacks to be referentially stable across draft mutations
  // so the adapter doesn't get rebuilt mid-turn. `draftRef` lets
  // `getWorkflow` read the latest draft without becoming a dependency.
  const draftRef = useRef<WorkflowDraft>(draft);
  draftRef.current = draft;
  const getWorkflow = useCallback(() => draftRef.current, []);
  const onToolCall = useCallback(
    (call: ToolCall) => applyCall(call),
    [applyCall],
  );

  return (
    <div className="relative isolate -mx-4 -my-6 flex h-[calc(100%+3rem)] sm:-mx-6 lg:-mx-8">
      <main className="flex h-full min-h-0 flex-1 flex-col gap-3 p-3">
        <header className="flex items-center gap-3 px-2">
          <h1 className="text-base font-semibold text-fg">Playground</h1>
          <span className="text-sm text-fg-3">
            {draft.name === "untitled" ? "untitled workflow" : draft.name}
          </span>
          <div className="ml-auto flex items-center gap-2">
            <DownloadButton draft={draft} />
            {!isChatOpen && (
              <button
                type="button"
                onClick={() => setChatOpen(true)}
                className="inline-flex items-center gap-1.5 rounded-md bg-overlay px-2.5 py-1.5 text-sm font-medium text-fg-2 ring-1 ring-line-strong transition-colors hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
              >
                <SparklesIcon className="size-4 text-teal-300" />
                Ask Fabro
              </button>
            )}
          </div>
        </header>

        <div className="grid min-h-0 flex-1 grid-rows-[3fr_2fr] gap-3">
          <div className="grid min-h-0 grid-cols-[1fr_220px] gap-3">
            <PlaygroundCanvas draft={draft} simulation={sim.state} />
            <aside className="flex h-full min-h-0 flex-col overflow-hidden rounded-md border border-line bg-panel-alt/40">
              <div className="flex shrink-0 items-center justify-between border-b border-line px-3 py-2">
                <span className="font-mono text-[10.5px] uppercase tracking-wider text-fg-muted">
                  Run trace
                </span>
              </div>
              <div className="min-h-0 flex-1 overflow-auto">
                <RunTrace state={sim.state} />
              </div>
              <div className="shrink-0 border-t border-line p-2">
                <SimulationControls sim={sim} />
              </div>
            </aside>
          </div>
          <FileTabs draft={draft} />
        </div>
      </main>

      <PlaygroundChatSidebar
        isOpen={isChatOpen}
        onClose={() => setChatOpen(false)}
        chatEndpoint={chatEndpoint}
        getWorkflow={getWorkflow}
        onToolCall={onToolCall}
        width={sidebarWidth}
        onWidthChange={setSidebarWidth}
      />
    </div>
  );
}
