import type { SimulationState } from "../canvas/simulation";

/**
 * Tiny live log next to the canvas during a simulated run. Mirrors the
 * cadence of the explainer's "RUN TRACE" pane: one mono line per step,
 * timestamped, with the active one highlighted.
 */
export default function RunTrace({ state }: { state: SimulationState }) {
  if (state.trace.length === 0) {
    return (
      <p className="px-3 py-2 font-mono text-[11px] text-fg-muted">
        Press <span className="text-fg-2">Simulate</span> to walk this graph.
      </p>
    );
  }
  return (
    <ul className="flex flex-col gap-0.5 px-2 py-2 font-mono text-[11px]">
      {state.trace.map((step) => {
        const isActive = state.active === step.nodeId && !state.finished;
        const isDone = !isActive;
        return (
          <li
            key={step.index}
            className={[
              "flex items-baseline gap-2 rounded px-2 py-1 transition-colors",
              isActive && "bg-teal-500/10 text-teal-200 ring-1 ring-teal-500/30",
              isDone && "text-fg-3",
            ]
              .filter(Boolean)
              .join(" ")}
          >
            <span className="text-fg-muted tabular-nums">
              {formatElapsed(step.elapsedMs)}
            </span>
            <span className="truncate">
              <span className="text-fg-muted">{step.nodeId}</span>
              {step.label !== step.nodeId && (
                <span className="ml-1.5 text-fg-2">{step.label}</span>
              )}
            </span>
          </li>
        );
      })}
      {state.finished && (
        <li className="px-2 py-1 text-mint">— done —</li>
      )}
    </ul>
  );
}

function formatElapsed(ms: number): string {
  const s = (ms / 1000).toFixed(1);
  return `${s.padStart(5, " ")}s`;
}
