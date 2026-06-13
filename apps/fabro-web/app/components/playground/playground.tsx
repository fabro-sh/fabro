import { useCallback, useMemo, useRef, useState } from "react";
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
import NodeInspector from "./ui/node-inspector";
import ResetButton from "./ui/reset-button";
import RunForRealButton, { type RealRunRedirect } from "./ui/run-for-real-button";
import RunTrace from "./ui/run-trace";
import SimulationControls from "./ui/simulation-controls";
import WorkflowHeader from "./ui/workflow-header";

export type PlaygroundAuthMode = "required" | "anonymous";

export type PlaygroundProps = {
  /**
   * URL the chat adapter posts each turn against. Externalised so the same
   * component tree can re-embed against a public, rate-limited variant of
   * the endpoint later.
   */
  chatEndpoint: string;
  /**
   * `required` — assume the parent shell has already enforced authentication
   * (current fabro-web routes do this via `AppShell`).
   * `anonymous` — anonymous embed mode for non-authenticated contexts; not
   * used by fabro-web today.
   */
  authMode: PlaygroundAuthMode;
  /**
   * Override the "Run for real" button to redirect somewhere instead of
   * opening the in-page modal that POSTs to `/api/v1/runs`. Set this in
   * embed contexts where the visitor has no project to run against, to
   * send them to a CTA URL such as `/download`. When unset, the button
   * uses the default in-page launch flow.
   */
  realRunRedirect?: RealRunRedirect;
};

/**
 * The playground feature surface, deliberately framed as a standalone
 * component tree so it can later be re-embedded as a self-contained React
 * subtree in other contexts. It must not depend on `AppShell`, react-router
 * context, or any of fabro-web's app-wide stores; any cross-cutting concern
 * (chat endpoint, auth mode, theme) flows in through props.
 *
 * Layout mirrors `/ask-fabro`: workspace on the left, a docked chat
 * column on the right that drives the canvas via streamed tool calls.
 */
export default function Playground({
  chatEndpoint,
  authMode: _authMode,
  realRunRedirect,
}: PlaygroundProps) {
  const { draft, applyCall, reset } = usePlaygroundDraft();
  const [isChatOpen, setChatOpen] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(SIDEBAR_WIDTH);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const sim = useSimulation(draft);

  // The selected node still exists in the draft, right? After a write
  // we may have deleted it — keep selection consistent.
  const selectedNode = useMemo(
    () =>
      selectedNodeId
        ? draft.nodes.find((n) => n.id === selectedNodeId) ?? null
        : null,
    [draft.nodes, selectedNodeId],
  );

  const handleReset = useCallback(() => {
    setSelectedNodeId(null);
    reset();
  }, [reset]);

  // The chat adapter is memoized on (chatEndpoint, getWorkflow, dispatch,
  // onParseFailure); we need every callback to be referentially stable
  // across draft mutations so the adapter doesn't get rebuilt mid-turn.
  // `draftRef` lets `getWorkflow` read the latest draft without becoming a
  // dependency.
  const draftRef = useRef<WorkflowDraft>(draft);
  draftRef.current = draft;
  const getWorkflow = useCallback(() => draftRef.current, []);
  const dispatch = useCallback(
    (call: ToolCall) => applyCall(call),
    [applyCall],
  );
  const onParseFailure = useCallback(
    (info: { message: string; rawContent: string }) => {
      // For now: log to the dev console. The chat itself already
      // surfaces "1 tool call, 1 with an error" via the tool-call
      // summary's `isError` flag, which is the user-visible signal
      // that something went wrong. A future iteration could auto-
      // submit a follow-up turn that nudges the model to re-emit
      // valid DOT.
      console.warn("[playground] failed to parse workflow file:", info.message);
    },
    [],
  );

  return (
    <div className="relative isolate -mx-4 -my-6 flex h-[calc(100%+3rem)] sm:-mx-6 lg:-mx-8">
      <main className="flex h-full min-h-0 flex-1 flex-col gap-3 p-3">
        <header className="flex items-center gap-3 px-2">
          <WorkflowHeader draft={draft} />
          <div className="ml-auto flex items-center gap-2">
            <ResetButton onReset={handleReset} />
            <DownloadButton draft={draft} />
            <RunForRealButton draft={draft} redirect={realRunRedirect} />
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
          <div className="grid min-h-0 grid-cols-[1fr_320px] gap-3">
            <PlaygroundCanvas
              draft={draft}
              simulation={sim.state}
              selectedNodeId={selectedNodeId}
              onSelectNode={setSelectedNodeId}
            />
            <aside className="flex h-full min-h-0 flex-col overflow-hidden rounded-md border border-line bg-panel-alt/40">
              {selectedNode ? (
                <NodeInspector
                  node={selectedNode}
                  draft={draft}
                  onClose={() => setSelectedNodeId(null)}
                />
              ) : (
                <>
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
                </>
              )}
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
        dispatch={dispatch}
        onParseFailure={onParseFailure}
        width={sidebarWidth}
        onWidthChange={setSidebarWidth}
      />
    </div>
  );
}
