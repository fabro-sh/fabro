export function BlockedRunNotice({
  questionText,
  cancelling = false,
  onCancel,
}: {
  questionText?: string | null;
  cancelling?: boolean;
  onCancel: () => void;
}) {
  return (
    <div
      role="status"
      className="mb-6 rounded-lg border border-amber/30 bg-amber/10 px-4 py-4 text-sm text-fg-2"
    >
      <p className="font-medium text-fg">This run is waiting for input.</p>
      <p className="mt-2 leading-6">
        {questionText?.trim()
          ? questionText
          : "Fabro is blocked on a human-in-the-loop question. Answer it from the CLI to continue the run."}
      </p>
      <p className="mt-2 text-fg-muted">
        If you don't want to continue in the CLI, you can cancel the run here instead.
      </p>
      <button
        type="button"
        onClick={onCancel}
        disabled={cancelling}
        className="mt-3 inline-flex min-h-12 items-center rounded-md px-3 text-sm font-medium text-fg-muted transition-colors hover:bg-amber/10 hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60"
      >
        {cancelling ? "Cancelling…" : "Cancel run anyway."}
      </button>
    </div>
  );
}
