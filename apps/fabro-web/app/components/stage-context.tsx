import type { StageContextData } from "./stage-renderers/helpers";
import { CodeBlock, JsonBlock } from "./stage-renderers/primitives";

function ContextValue({ value }: { value: unknown }) {
  if (typeof value === "string") {
    return <CodeBlock>{value}</CodeBlock>;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return <span className="font-mono text-sm text-fg-3">{String(value)}</span>;
  }
  if (value === null) {
    return <span className="font-mono text-sm text-fg-muted">null</span>;
  }
  return <JsonBlock value={JSON.stringify(value, null, 2)} />;
}

/**
 * Renders the workflow's deliberate outputs for a single stage visit: the
 * routing hints it emitted and the context keys it set. Engine bookkeeping
 * keys are already filtered out by `extractStageContext`.
 */
export function StageContext({ data }: { data: StageContextData }) {
  const { preferredLabel, suggestedNextIds } = data.routing;
  const hasRouting = preferredLabel != null || suggestedNextIds.length > 0;
  const updateKeys = Object.keys(data.updates).sort();

  return (
    <div className="space-y-6 pl-3 pr-4 sm:pr-6 lg:pr-8">
      {hasRouting && (
        <section>
          <h3 className="mb-2 text-xs font-medium uppercase tracking-wider text-fg-muted">
            Routing
          </h3>
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
            {preferredLabel != null && (
              <>
                <dt className="text-fg-muted">Preferred edge</dt>
                <dd className="font-mono text-fg-3">{preferredLabel}</dd>
              </>
            )}
            {suggestedNextIds.length > 0 && (
              <>
                <dt className="text-fg-muted">Suggested next</dt>
                <dd className="font-mono text-fg-3">{suggestedNextIds.join(", ")}</dd>
              </>
            )}
          </dl>
        </section>
      )}

      {updateKeys.length > 0 && (
        <section>
          <h3 className="mb-2 text-xs font-medium uppercase tracking-wider text-fg-muted">
            Context writes
          </h3>
          <div className="space-y-4">
            {updateKeys.map((key) => (
              <div key={key}>
                <div className="mb-1 font-mono text-xs text-fg-2">{key}</div>
                <ContextValue value={data.updates[key]} />
              </div>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
