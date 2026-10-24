import type { FailureDetail } from "@qltysh/fabro-api-client";
import { CopyButton } from "../../components/ui";

/** Run diagnostics are independent of the command or adapter that failed. */
export function RunFailureDetails({ failure }: { failure?: FailureDetail | null }) {
  if (!failure) return null;
  const output = failure.exec_output_tail;

  const streams = [
    { label: "Standard error", text: output?.stderr, truncated: output?.stderr_truncated },
    { label: "Standard output", text: output?.stdout, truncated: output?.stdout_truncated },
  ].filter((stream) => stream.text || stream.truncated);
  if (streams.length === 0 && !failure.causes?.length) return null;

  return (
    <div className="mb-6 min-w-0 shrink-0 text-sm">
      <p className="break-words text-fg">{failure.message}</p>
      <details className="mt-2">
        <summary className="w-fit cursor-pointer rounded text-fg-2 outline-teal-500 hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2">
          Show error details
        </summary>
        <div className="mt-3 max-h-80 space-y-4 overflow-auto rounded-lg bg-panel p-4">
          {failure.causes?.length ? (
            <section aria-label="Causes">
              <h3 className="mb-1 font-medium text-fg-2">Causes</h3>
              <ul className="list-inside list-disc space-y-1 break-words text-fg">
                {failure.causes.map((cause, index) => <li key={index}>{cause}</li>)}
              </ul>
            </section>
          ) : null}
          {streams.map(({ label, text, truncated }) => (
            <section key={label} aria-label={label}>
              <div className="mb-1 flex items-center gap-2 font-medium text-fg-2">
                <h3>{label}</h3>
                {text && <CopyButton value={text} label={`Copy ${label.toLowerCase()}`} />}
              </div>
              {truncated && <p className="mb-2 text-fg-3">Earlier output was omitted.</p>}
              {text && <pre className="whitespace-pre-wrap break-words font-mono text-xs text-fg">{text}</pre>}
            </section>
          ))}
        </div>
      </details>
    </div>
  );
}
