import { useState } from "react";
import { SparklesIcon } from "@heroicons/react/24/solid";
import {
  ArrowTopRightOnSquareIcon,
  ChartBarIcon,
  CheckCircleIcon,
  ClockIcon,
} from "@heroicons/react/24/outline";

import AskFabroSidebar, {
  SIDEBAR_WIDTH,
} from "../components/chats/ask-fabro-sidebar";

// hideHeader/fullHeight/wide mirror the chats layout so the demo workspace and
// the docked Ask-Fabro sidebar bleed edge-to-edge below the top nav.
export const handle = { hideHeader: true, fullHeight: true, wide: true };

/**
 * `/ask-fabro` — prototype page for the right-docked "Ask Fabro" assistant.
 * The workspace content is placeholder; its only purpose is to host the
 * trigger button. The assistant streams scripted, fake replies — no real AI.
 */
export default function AskFabro() {
  const [isOpen, setIsOpen] = useState(false);
  const [width, setWidth] = useState(SIDEBAR_WIDTH);

  return (
    // Cancel AppShell's content padding (px-4 py-6 / sm:px-6 / lg:px-8) so the
    // workspace and sidebar bleed edge-to-edge below the top nav; the height
    // grows by the vertical padding amount to recover the full viewport area.
    <div className="relative isolate -mx-4 -my-6 flex h-[calc(100%+3rem)] sm:-mx-6 lg:-mx-8">
      <DemoWorkspace isOpen={isOpen} onOpen={() => setIsOpen(true)} />
      <AskFabroSidebar
        isOpen={isOpen}
        onClose={() => setIsOpen(false)}
        runId="demo"
        width={width}
        onWidthChange={setWidth}
      />
    </div>
  );
}

function DemoWorkspace({
  isOpen,
  onOpen,
}: {
  isOpen: boolean;
  onOpen: () => void;
}) {
  return (
    <div className="flex h-full min-h-0 flex-1 flex-col">
      <header className="flex h-14 shrink-0 items-center gap-4 border-b border-line bg-panel/40 px-6 backdrop-blur-sm">
        <div className="flex items-baseline gap-3">
          <h1 className="text-base font-semibold text-fg">Runs</h1>
          <span className="text-sm text-fg-3">fabro-web · main</span>
        </div>
        <button
          type="button"
          onClick={onOpen}
          disabled={isOpen}
          className="ml-auto inline-flex items-center gap-1.5 rounded-md bg-overlay px-2.5 py-1.5 text-sm font-medium text-fg-2 ring-1 ring-line-strong transition-colors hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-50"
        >
          <SparklesIcon className="size-4 text-teal-300" />
          Ask Fabro
        </button>
      </header>

      <main className="min-h-0 flex-1 overflow-y-auto p-6">
        <div className="mx-auto max-w-4xl">
          <div className="grid grid-cols-3 gap-4">
            <StatCard
              Icon={CheckCircleIcon}
              label="Succeeded"
              value="142"
              accent="text-mint"
            />
            <StatCard
              Icon={ClockIcon}
              label="Running"
              value="6"
              accent="text-amber"
            />
            <StatCard
              Icon={ChartBarIcon}
              label="Avg duration"
              value="2m 14s"
              accent="text-teal-300"
            />
          </div>

          <h2 className="mt-10 mb-3 text-sm font-medium text-fg-3">
            Recent runs
          </h2>
          <ul
            className="overflow-hidden rounded-lg bg-panel/40 ring-1 ring-line"
          >
            {RUN_ROWS.map((row, i) => (
              <li
                key={row.id}
                className={[
                  "flex items-center gap-4 px-4 py-3 text-sm",
                  i !== RUN_ROWS.length - 1 && "border-b border-line",
                ]
                  .filter(Boolean)
                  .join(" ")}
              >
                <span
                  className={`inline-block size-2 shrink-0 rounded-full ${row.dot}`}
                />
                <span className="font-mono text-xs text-fg-3">{row.id}</span>
                <span className="truncate text-fg-2">{row.title}</span>
                <span className="ml-auto shrink-0 text-xs text-fg-muted tabular-nums">
                  {row.duration}
                </span>
                <ArrowTopRightOnSquareIcon className="size-4 shrink-0 text-fg-muted" />
              </li>
            ))}
          </ul>
        </div>
      </main>
    </div>
  );
}

function StatCard({
  Icon,
  label,
  value,
  accent,
}: {
  Icon: React.ComponentType<{ className?: string }>;
  label: string;
  value: string;
  accent: string;
}) {
  return (
    <div className="rounded-lg bg-panel/40 p-4 ring-1 ring-line">
      <div
        className={`flex items-center gap-2 text-xs font-medium uppercase tracking-wider ${accent}`}
      >
        <Icon className="size-3.5" />
        {label}
      </div>
      <div className="mt-2 text-2xl font-semibold text-fg tabular-nums">
        {value}
      </div>
    </div>
  );
}

const RUN_ROWS = [
  { id: "r_8f2a", title: "Update copy on the landing page", duration: "1m 42s", dot: "bg-mint" },
  { id: "r_8f29", title: "Refactor the useChat hook", duration: "3m 08s", dot: "bg-mint" },
  { id: "r_8f28", title: "Investigate flaky test in fabro-server", duration: "2m 51s", dot: "bg-amber" },
  { id: "r_8f27", title: "Bump assistant-ui to latest", duration: "0m 47s", dot: "bg-mint" },
  { id: "r_8f26", title: "Generate release notes for v0.42", duration: "1m 19s", dot: "bg-mint" },
  { id: "r_8f25", title: "Audit unused dependencies", duration: "5m 33s", dot: "bg-coral" },
];
