import { useMemo, type ReactNode } from "react";

import { highlightJson } from "../event-debug-helpers";
import { prettyJson } from "./pretty-json";

export { Markdown } from "../markdown";

export function DetailField({
  label,
  children,
  mono = false,
}: {
  label: string;
  children: ReactNode;
  mono?: boolean;
}) {
  return (
    <div>
      <div className="mb-1 text-xs font-medium uppercase tracking-wider text-fg-muted">
        {label}
      </div>
      <div className={mono ? "font-mono text-sm text-fg-3" : "text-sm text-fg-3"}>
        {children}
      </div>
    </div>
  );
}

export function CodeBlock({ children }: { children: string }) {
  return (
    <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
      {children || <span className="text-fg-muted">empty</span>}
    </pre>
  );
}

export function JsonBlock({ value }: { value: string }) {
  const pretty = useMemo(() => prettyJson(value), [value]);
  const tokens = useMemo(
    () => (pretty.isJson ? highlightJson(pretty.text) : null),
    [pretty.isJson, pretty.text],
  );
  return (
    <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
      {!pretty.text ? (
        <span className="text-fg-muted">empty</span>
      ) : (
        tokens ?? pretty.text
      )}
    </pre>
  );
}
