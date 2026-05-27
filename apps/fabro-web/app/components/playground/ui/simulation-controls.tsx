import {
  ArrowPathIcon,
  PlayIcon,
} from "@heroicons/react/24/solid";

import {
  MAX_STEP_MS,
  MIN_STEP_MS,
  type PlaygroundSimulation,
} from "../canvas/use-simulation";

/**
 * Play / Reset buttons + speed slider. The Play button is disabled when
 * the draft is the welcome state (start → ??? → exit) — there's nothing
 * to walk yet. The slider is the explainer's "speed" control: 500ms
 * (snappy) to 3000ms (slow tour).
 */
export default function SimulationControls({
  sim,
}: {
  sim: PlaygroundSimulation;
}) {
  return (
    <div className="flex items-center gap-3">
      <button
        type="button"
        onClick={sim.play}
        disabled={!sim.isPlayable || sim.isRunning}
        title={
          sim.isPlayable
            ? "Simulate a walk through the workflow"
            : "Add a node first"
        }
        className="inline-flex items-center gap-1.5 rounded-md bg-overlay px-2.5 py-1.5 text-sm font-medium text-fg-2 ring-1 ring-line-strong transition-colors hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-50"
      >
        <PlayIcon className="size-3.5 text-teal-300" />
        {sim.isRunning ? "Running…" : "Simulate"}
      </button>
      <button
        type="button"
        onClick={sim.reset}
        disabled={sim.state.trace.length === 0}
        title="Reset simulation"
        className="inline-flex size-7 items-center justify-center rounded-md text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:cursor-not-allowed disabled:opacity-40"
      >
        <ArrowPathIcon className="size-3.5" />
      </button>

      <div className="flex items-center gap-2 text-[11px] text-fg-muted">
        <span className="font-mono">speed</span>
        <input
          type="range"
          min={MIN_STEP_MS}
          max={MAX_STEP_MS}
          step={250}
          // Slider feels natural when "right = fast"; invert via max+min-value.
          value={MAX_STEP_MS + MIN_STEP_MS - sim.stepMs}
          onChange={(e) =>
            sim.setStepMs(
              MAX_STEP_MS + MIN_STEP_MS - Number.parseInt(e.currentTarget.value, 10),
            )
          }
          className="h-1 w-24 cursor-pointer accent-teal-500"
          aria-label="Simulation speed"
        />
      </div>
    </div>
  );
}
