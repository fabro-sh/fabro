import { Link } from "react-router";
import {
  smoothnessConfig,
  learningCategoryConfig,
  frictionKindConfig,
  openItemKindConfig,
  formatDurationMs,
} from "../data/retros";
import type { Retro } from "../data/retros";
import { apiJson } from "../api";

export async function loader({ request, params }: any) {
  const retro = await apiJson<Retro>(`/runs/${params.id}/retro`, { request });
  return { retro };
}

export function meta({ data }: any) {
  const retro = data?.retro;
  return [{ title: retro ? `Retro: ${retro.goal} \u2014 Fabro` : "Retro \u2014 Fabro" }];
}

function formatCost(cost: number | undefined): string {
  if (cost == null) return "--";
  return `$${cost.toFixed(2)}`;
}

export default function RunRetro({ loaderData }: any) {
  const { retro } = loaderData;

  if (!retro) {
    return <p className="py-8 text-center text-sm text-fg-muted">No retrospective found for this run.</p>;
  }

  const smoothness = retro.smoothness ? smoothnessConfig[retro.smoothness] : null;

  return (
    <div className="space-y-6">
      {/* Smoothness + Summary Header */}
      <div className="flex items-start gap-4">
        {smoothness && (
          <span className={`inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-semibold ${smoothness.bg} ${smoothness.text}`}>
            <span className={`size-2.5 rounded-full ${smoothness.dot}`} />
            {smoothness.label}
          </span>
        )}
        <div className="min-w-0 flex-1">
          <p className="text-sm text-fg-3">{retro.goal}</p>
          <p className="mt-1 font-mono text-xs text-fg-muted">
            {retro.workflow_name} &middot; {new Date(retro.timestamp).toLocaleString()}
          </p>
        </div>
      </div>

      {/* Aggregate Stats */}
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <StatCard label="Duration" value={formatDurationMs(retro.stats.total_duration_ms)} />
        <StatCard label="Cost" value={formatCost(retro.stats.total_cost)} />
        <StatCard label="Retries" value={String(retro.stats.total_retries)} warn={retro.stats.total_retries > 0} />
        <StatCard label="Files" value={String(retro.stats.files_touched.length)} />
      </div>

      {/* Intent + Outcome */}
      {(retro.intent ?? retro.outcome) && (
        <div className="grid gap-3 sm:grid-cols-2">
          {retro.intent && (
            <div className="rounded-md border border-line bg-panel/60 p-4">
              <h3 className="text-xs font-medium uppercase tracking-wider text-fg-muted">Intent</h3>
              <p className="mt-2 text-sm leading-relaxed text-fg-3">{retro.intent}</p>
            </div>
          )}
          {retro.outcome && (
            <div className="rounded-md border border-line bg-panel/60 p-4">
              <h3 className="text-xs font-medium uppercase tracking-wider text-fg-muted">Outcome</h3>
              <p className="mt-2 text-sm leading-relaxed text-fg-3">{retro.outcome}</p>
            </div>
          )}
        </div>
      )}

      {/* Learnings */}
      {retro.learnings && retro.learnings.length > 0 && (
        <div>
          <h3 className="mb-3 text-xs font-medium uppercase tracking-wider text-fg-muted">Learnings</h3>
          <div className="space-y-2">
            {retro.learnings.map((learning, i) => {
              const config = learningCategoryConfig[learning.category];
              return (
                <div key={learning.text} className="flex items-start gap-3 rounded-md border border-line bg-panel/60 px-4 py-3">
                  <span className={`mt-0.5 shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider ${config.text} bg-overlay`}>
                    {config.label}
                  </span>
                  <p className="text-sm leading-relaxed text-fg-3">{learning.text}</p>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Friction Points */}
      {retro.friction_points && retro.friction_points.length > 0 && (
        <div>
          <h3 className="mb-3 text-xs font-medium uppercase tracking-wider text-fg-muted">Friction Points</h3>
          <div className="space-y-2">
            {retro.friction_points.map((fp, i) => {
              const config = frictionKindConfig[fp.kind];
              return (
                <div key={fp.description} className="flex items-start gap-3 rounded-md border border-line bg-panel/60 px-4 py-3">
                  <span className={`mt-0.5 shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider ${config.text} bg-overlay`}>
                    {config.label}
                  </span>
                  <div className="min-w-0 flex-1">
                    <p className="text-sm leading-relaxed text-fg-3">{fp.description}</p>
                    {fp.stage_id && (
                      <p className="mt-1 font-mono text-xs text-fg-muted">
                        Stage: {retro.stages.find((s) => s.stage_id === fp.stage_id)?.stage_label ?? fp.stage_id}
                      </p>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Open Items */}
      {retro.open_items && retro.open_items.length > 0 && (
        <div>
          <h3 className="mb-3 text-xs font-medium uppercase tracking-wider text-fg-muted">Open Items</h3>
          <div className="space-y-2">
            {retro.open_items.map((item, i) => {
              const config = openItemKindConfig[item.kind];
              return (
                <div key={item.description} className="flex items-start gap-3 rounded-md border border-line bg-panel/60 px-4 py-3">
                  <span className={`mt-0.5 shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider ${config.text} bg-overlay`}>
                    {config.label}
                  </span>
                  <p className="text-sm leading-relaxed text-fg-3">{item.description}</p>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Stage Breakdown */}
      <div>
        <h3 className="mb-3 text-xs font-medium uppercase tracking-wider text-fg-muted">Stage Breakdown</h3>
        <div className="rounded-md border border-line overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-line bg-panel/60 text-left text-xs text-fg-muted">
                <th className="px-4 py-2.5 font-medium">Stage</th>
                <th className="px-4 py-2.5 font-medium">Status</th>
                <th className="px-4 py-2.5 font-medium text-right">Duration</th>
                <th className="px-4 py-2.5 font-medium text-right">Retries</th>
                <th className="px-4 py-2.5 font-medium text-right">Cost</th>
                <th className="px-4 py-2.5 font-medium text-right">Files</th>
              </tr>
            </thead>
            <tbody>
              {retro.stages.map((stage) => (
                <tr key={stage.stage_id} className="border-b border-line last:border-b-0">
                  <td className="px-4 py-3">
                    <Link
                      to={`/runs/${retro.run_id}/stages/${stage.stage_id}`}
                      className="text-fg-2 hover:text-fg"
                    >
                      {stage.stage_label}
                    </Link>
                    {stage.notes && (
                      <p className="mt-1 text-xs text-fg-muted">{stage.notes}</p>
                    )}
                    {stage.failure_reason && (
                      <p className="mt-1 text-xs text-coral/80">{stage.failure_reason}</p>
                    )}
                  </td>
                  <td className="px-4 py-3">
                    <StageStatusBadge status={stage.status} />
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                    {formatDurationMs(stage.duration_ms)}
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs tabular-nums">
                    <span className={stage.retries > 0 ? "text-amber" : "text-fg-3"}>
                      {stage.retries}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                    {formatCost(stage.cost)}
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                    {stage.files_touched.length}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}

function StatCard({ label, value, warn }: { label: string; value: string; warn?: boolean }) {
  return (
    <div className="rounded-md border border-line bg-panel/60 px-4 py-3">
      <p className="text-xs font-medium uppercase tracking-wider text-fg-muted">{label}</p>
      <p className={`mt-1 font-mono text-lg font-semibold tabular-nums ${warn ? "text-amber" : "text-fg"}`}>
        {value}
      </p>
    </div>
  );
}

function StageStatusBadge({ status }: { status: string }) {
  const styles: Record<string, string> = {
    completed: "text-mint",
    running: "text-teal-500",
    pending: "text-fg-muted",
    failed: "text-coral",
    cancelled: "text-fg-muted",
  };
  const colorClass = styles[status] ?? "text-fg-3";
  return (
    <span className={`text-xs font-medium capitalize ${colorClass}`}>
      {status}
    </span>
  );
}
