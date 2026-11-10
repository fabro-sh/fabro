import type { ReactNode } from "react";
import {
  Virtualizer,
  WorkerPoolContextProvider,
} from "@pierre/diffs/react";

import { workerFactory } from "../../lib/pierre-diffs-worker";
import { PIERRE_HIGHLIGHTER_OPTIONS } from "../../lib/pierre-theme";

const poolOptions = { workerFactory };

export function VirtualizedDiffList({ children }: { children: ReactNode }) {
  return (
    <WorkerPoolContextProvider
      poolOptions={poolOptions}
      highlighterOptions={PIERRE_HIGHLIGHTER_OPTIONS}
    >
      <Virtualizer
        className="min-h-0 flex-1 overflow-auto pr-2"
        contentClassName="flex flex-col gap-2 pb-[calc(1rem+var(--fabro-interview-dock-clearance,0px))]"
      >
        {children}
      </Virtualizer>
    </WorkerPoolContextProvider>
  );
}
