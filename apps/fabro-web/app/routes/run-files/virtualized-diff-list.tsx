import type { ReactNode } from "react";
import {
  Virtualizer,
  WorkerPoolContextProvider,
} from "@pierre/diffs/react";

import { workerFactory } from "../../lib/pierre-diffs-worker";

const poolOptions = { workerFactory };
const highlighterOptions = { theme: "pierre-dark" };

export function VirtualizedDiffList({ children }: { children: ReactNode }) {
  return (
    <WorkerPoolContextProvider
      poolOptions={poolOptions}
      highlighterOptions={highlighterOptions}
    >
      <Virtualizer
        className="min-h-0 flex-1 overflow-auto pr-2"
        contentClassName="flex flex-col gap-2 pb-[calc(1rem+var(--fabro-interview-dock-clearance,0px))]"
        contentStyle={{
          transition:
            "var(--fabro-interview-dock-clearance-transition, padding 300ms cubic-bezier(0.16, 1, 0.3, 1))",
        }}
      >
        {children}
      </Virtualizer>
    </WorkerPoolContextProvider>
  );
}
