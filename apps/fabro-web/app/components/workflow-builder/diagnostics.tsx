import { ExclamationTriangleIcon, XCircleIcon } from "@heroicons/react/16/solid";

import type { Diagnostic } from "../../lib/workflow-builder/types";

export function countDiagnostics(diagnostics: Diagnostic[]): { errors: number; warnings: number } {
  let errors = 0;
  let warnings = 0;
  for (const diagnostic of diagnostics) {
    if (diagnostic.severity === "error") errors += 1;
    else warnings += 1;
  }
  return { errors, warnings };
}

export function diagnosticSummary(diagnostics: Diagnostic[]): string {
  const { errors, warnings } = countDiagnostics(diagnostics);
  const parts: string[] = [];
  if (errors > 0) parts.push(`${errors} error${errors === 1 ? "" : "s"}`);
  if (warnings > 0) parts.push(`${warnings} warning${warnings === 1 ? "" : "s"}`);
  return parts.length > 0 ? parts.join(", ") : "No issues";
}

function DiagnosticIcon({ severity }: { severity: Diagnostic["severity"] }) {
  return severity === "error" ? (
    <XCircleIcon className="wfb-diagnostic-icon wfb-diagnostic-error size-4 shrink-0" aria-hidden="true" />
  ) : (
    <ExclamationTriangleIcon className="wfb-diagnostic-icon wfb-diagnostic-warning size-4 shrink-0" aria-hidden="true" />
  );
}

function location(diagnostic: Diagnostic): string | null {
  if (diagnostic.line !== undefined) {
    return diagnostic.column !== undefined
      ? `Line ${diagnostic.line}, column ${diagnostic.column}`
      : `Line ${diagnostic.line}`;
  }
  return diagnostic.nodeId ?? diagnostic.edgeId ?? null;
}

/** A list of problems. With `onPick`, each one jumps to where it applies. */
export function DiagnosticsList({
  diagnostics,
  onPick,
}: {
  diagnostics: Diagnostic[];
  onPick?: (diagnostic: Diagnostic) => void;
}) {
  if (diagnostics.length === 0) return <p className="wfb-help">No issues found.</p>;
  const ordered = [...diagnostics].sort(
    (a, b) => Number(b.severity === "error") - Number(a.severity === "error"),
  );
  return (
    <ul className="flex flex-col">
      {ordered.map((diagnostic, index) => {
        const where = location(diagnostic);
        const body = (
          <>
            <DiagnosticIcon severity={diagnostic.severity} />
            <span className="min-w-0 flex-1">
              <span className="wfb-diagnostic-message">{diagnostic.message}</span>
              {where ? <span className="wfb-mono wfb-diagnostic-where">{where}</span> : null}
            </span>
          </>
        );
        const canPick =
          onPick &&
          (diagnostic.line !== undefined || diagnostic.nodeId !== undefined || diagnostic.edgeId !== undefined);
        return (
          // Two diagnostics can read the same, so the index keeps keys unique.
          <li key={`${index}:${diagnostic.message}`}>
            {canPick ? (
              <button type="button" className="wfb-diagnostic" onClick={() => onPick(diagnostic)}>
                {body}
              </button>
            ) : (
              <div className="wfb-diagnostic">{body}</div>
            )}
          </li>
        );
      })}
    </ul>
  );
}
