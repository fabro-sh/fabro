import type { EventEnvelope } from "@qltysh/fabro-api-client";

import type { Stage } from "../stage-sidebar";
import {
  debugCategory,
  debugCategoryLabel,
  debugCategoryTone,
  type DebugCategory,
} from "../event-debug-helpers";
import { StageMetaBar } from "./meta-bar";

interface CategoryCount {
  category: DebugCategory;
  count: number;
}

function summarizeEventCategories(events: EventEnvelope[]): CategoryCount[] {
  const counts = new Map<DebugCategory, number>();
  for (const event of events) {
    if (!event.event) continue;
    const cat = debugCategory(event.event);
    counts.set(cat, (counts.get(cat) ?? 0) + 1);
  }
  return Array.from(counts.entries())
    .map(([category, count]) => ({ category, count }))
    .sort((a, b) => b.count - a.count);
}

export function StageSummary({
  stage,
  events,
}: {
  stage: Stage;
  events: EventEnvelope[];
}) {
  const categories = summarizeEventCategories(events);

  return (
    <div className="space-y-6 pl-3 pr-4 sm:pr-6 lg:pr-8">
      <StageMetaBar stage={stage} />

      <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
        <dt className="text-fg-muted">Stage</dt>
        <dd className="font-mono text-fg-3">{stage.name}</dd>
        <dt className="text-fg-muted">Node</dt>
        <dd className="font-mono text-fg-3">
          {stage.nodeId}
          {stage.visit > 1 && <span className="text-fg-muted"> · visit {stage.visit}</span>}
        </dd>
        <dt className="text-fg-muted">Stage ID</dt>
        <dd className="font-mono text-fg-3">{stage.id}</dd>
      </dl>

      <section>
        <h3 className="mb-2 text-xs font-medium uppercase tracking-wider text-fg-muted">
          Events
        </h3>
        {categories.length === 0 ? (
          <p className="text-sm text-fg-muted">No events recorded for this stage.</p>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {categories.map(({ category, count }) => (
              <li
                key={category}
                className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[11px] font-medium uppercase tracking-wider ${debugCategoryTone(category)}`}
              >
                <span>{debugCategoryLabel(category)}</span>
                <span className="font-mono tabular-nums opacity-80">{count}</span>
              </li>
            ))}
          </ul>
        )}
        <p className="mt-3 text-xs text-fg-muted">
          Switch to the Debug tab to inspect individual events.
        </p>
      </section>
    </div>
  );
}
