import { useState } from "react";
import { TrashIcon } from "@heroicons/react/24/outline";

/**
 * "Start over" — wipes the localStorage draft and resets the canvas back
 * to the welcome state. Confirms inline before firing so a misclick on
 * an actively-built graph doesn't silently torch the user's work.
 */
export default function ResetButton({ onReset }: { onReset: () => void }) {
  const [confirming, setConfirming] = useState(false);

  if (confirming) {
    return (
      <span className="inline-flex items-center gap-1 rounded-md bg-coral/10 px-2 py-1 text-xs text-coral ring-1 ring-coral/30">
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
      title="Start over with an empty workflow"
      className="inline-flex size-7 items-center justify-center rounded-md text-fg-muted transition-colors hover:bg-overlay hover:text-fg-2 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
      aria-label="Start over"
    >
      <TrashIcon className="size-3.5" />
    </button>
  );
}
