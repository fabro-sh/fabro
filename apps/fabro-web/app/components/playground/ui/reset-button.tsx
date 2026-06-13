import { useState } from "react";
import { ArrowPathIcon } from "@heroicons/react/24/outline";

/**
 * "Start over" — wipes the localStorage draft and resets the canvas
 * back to the welcome state. Confirms inline before firing so a
 * misclick on an actively-built graph doesn't silently torch the
 * user's work.
 */
export default function ResetButton({ onReset }: { onReset: () => void }) {
  const [confirming, setConfirming] = useState(false);

  if (confirming) {
    return (
      <span className="inline-flex items-center gap-1 rounded-md bg-coral/10 px-2 py-1 text-sm text-coral ring-1 ring-coral/30">
        <span>Start over?</span>
        <button
          type="button"
          onClick={() => {
            onReset();
            setConfirming(false);
          }}
          className="rounded px-1.5 py-0.5 font-medium ring-1 ring-coral/40 hover:bg-coral/20"
        >
          Yes
        </button>
        <button
          type="button"
          onClick={() => setConfirming(false)}
          className="rounded px-1.5 py-0.5 text-fg-muted hover:bg-overlay hover:text-fg-2"
        >
          Cancel
        </button>
      </span>
    );
  }

  return (
    <button
      type="button"
      onClick={() => setConfirming(true)}
      title="Wipe the canvas and start a new workflow"
      className="inline-flex items-center gap-1.5 rounded-md bg-overlay px-3 py-1.5 text-sm font-medium text-fg-2 ring-1 ring-line-strong transition-colors hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
    >
      <ArrowPathIcon className="size-4" />
      Start over
    </button>
  );
}
