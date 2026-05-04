import { useMemo } from "react";

import { EmptyState } from "../components/state";
import { formatDurationSecs } from "../lib/format";
import { useRunBilling } from "../lib/queries";
import { IN_FLIGHT_STAGE_STATES } from "../lib/stage-sidebar";
import { useTickingNow } from "../lib/time";
import type { RunBilling, RunBillingStage } from "@qltysh/fabro-api-client";

const EMPTY_VALUE = "—";

function formatTokens(n: number | null | undefined) {
  if (n == null) return EMPTY_VALUE;
  return `${(n / 1000).toFixed(1)}k`;
}

function formatUsdMicros(usdMicros?: number | null) {
  return usdMicros == null ? EMPTY_VALUE : `$${(usdMicros / 1_000_000).toFixed(2)}`;
}

function isInFlight(stage: RunBillingStage): boolean {
  return stage.state != null && IN_FLIGHT_STAGE_STATES.has(stage.state);
}

interface MappedStageRow {
  stage:          string;
  model:          string | null;
  inputTokens:    number | null;
  outputTokens:   number | null;
  runtimeSecs:    number;
  totalUsdMicros: number | null | undefined;
}

function liveRuntimeSecs(stage: RunBillingStage, now: number): number {
  if (stage.started_at) {
    const startedMs = new Date(stage.started_at).getTime();
    if (Number.isFinite(startedMs)) {
      return Math.max(0, (now - startedMs) / 1000);
    }
  }
  return stage.runtime_secs;
}

function mapStageRow(stage: RunBillingStage, runtimeSecs: number): MappedStageRow {
  const hasModel = stage.model != null;
  return {
    stage:          stage.stage.name,
    model:          stage.model?.id ?? null,
    inputTokens:    hasModel ? stage.billing.input_tokens : null,
    outputTokens:   hasModel
      ? stage.billing.output_tokens + stage.billing.reasoning_tokens
      : null,
    runtimeSecs,
    totalUsdMicros: stage.billing.total_usd_micros,
  };
}

export default function RunBilling({ params }: { params: { id: string } }) {
  const billingQuery = useRunBilling(params.id);
  const billing = billingQuery.data;
  const hasInFlight = billing?.stages.some(isInFlight) ?? false;

  // Tick once per second only while a stage is in-flight.
  const now = useTickingNow(hasInFlight);

  // Completed rows don't depend on `now`; memoize them by `billing` so we
  // don't reallocate them every tick.
  const completedRows = useMemo<MappedStageRow[]>(() => {
    if (!billing) return [];
    return billing.stages.map((stage) => mapStageRow(stage, stage.runtime_secs));
  }, [billing]);

  // The model breakdown is server-derived and stable across ticks too.
  const modelBreakdown = useMemo(() => {
    if (!billing) return [];
    return billing.by_model
      .map((entry) => ({
        model:          entry.model.id,
        stages:         entry.stages,
        inputTokens:    entry.billing.input_tokens,
        outputTokens:   entry.billing.output_tokens + entry.billing.reasoning_tokens,
        totalUsdMicros: entry.billing.total_usd_micros,
      }))
      .sort((a, b) => (b.totalUsdMicros ?? -1) - (a.totalUsdMicros ?? -1));
  }, [billing]);

  // Re-derive only the in-flight rows on each tick; everything else stays put.
  const rows = useMemo<MappedStageRow[]>(() => {
    if (!billing) return [];
    if (!hasInFlight) return completedRows;
    return billing.stages.map((stage, idx) =>
      isInFlight(stage)
        ? mapStageRow(stage, liveRuntimeSecs(stage, now))
        : completedRows[idx],
    );
  }, [billing, completedRows, hasInFlight, now]);

  // While ticking, sum the displayed row runtimes so the footer updates in
  // lock-step. Otherwise trust the server's authoritative total.
  const totalRuntimeSecs = hasInFlight
    ? rows.reduce((sum, row) => sum + row.runtimeSecs, 0)
    : (billing?.totals.runtime_secs ?? 0);

  const hasLlmStages = (billing?.by_model.length ?? 0) > 0;
  const totalInput = hasLlmStages ? (billing?.totals.input_tokens ?? null) : null;
  const totalOutput = hasLlmStages && billing
    ? billing.totals.output_tokens + billing.totals.reasoning_tokens
    : null;
  const totalUsdMicros = billing?.totals.total_usd_micros;
  const modelStageCount = modelBreakdown.reduce((sum, row) => sum + row.stages, 0);

  if (!rows.length) {
    return (
      <div className="py-12">
        <EmptyState
          title="No stages yet"
          description="Stages will appear as soon as the run starts executing."
        />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="overflow-hidden rounded-md border border-line">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-line bg-panel/60 text-left text-xs font-medium text-fg-3">
              <th className="px-4 py-2.5 font-medium">Stage</th>
              <th className="px-4 py-2.5 font-medium">Model</th>
              <th className="px-4 py-2.5 font-medium text-right">Tokens</th>
              <th className="px-4 py-2.5 font-medium text-right">Run time</th>
              <th className="px-4 py-2.5 font-medium text-right">Billing</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.stage} className="border-b border-line last:border-b-0">
                <td className="px-4 py-3 text-fg-2">{row.stage}</td>
                <td className="px-4 py-3 font-mono text-xs text-fg-3">
                  {row.model ?? EMPTY_VALUE}
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                  {formatTokens(row.inputTokens)} <span className="text-fg-muted">/</span>{" "}
                  {formatTokens(row.outputTokens)}
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">
                  {formatDurationSecs(row.runtimeSecs)}
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">
                  {formatUsdMicros(row.totalUsdMicros)}
                </td>
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr className="border-t border-line-strong bg-overlay">
              <td className="px-4 py-3 font-medium text-fg">Total</td>
              <td />
              <td className="px-4 py-3 text-right font-mono text-xs tabular-nums font-medium text-fg">
                {formatTokens(totalInput)} <span className="text-fg-muted">/</span>{" "}
                {formatTokens(totalOutput)}
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">
                {formatDurationSecs(totalRuntimeSecs)}
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">
                {formatUsdMicros(totalUsdMicros)}
              </td>
            </tr>
          </tfoot>
        </table>
      </div>

      {modelBreakdown.length > 0 ? (
        <div>
          <h3 className="mb-3 text-sm font-semibold text-fg">By model</h3>
          <div className="overflow-hidden rounded-md border border-line">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-line bg-panel/60 text-left text-xs font-medium text-fg-3">
                  <th className="px-4 py-2.5 font-medium">Model</th>
                  <th className="px-4 py-2.5 font-medium text-right">Stages</th>
                  <th className="px-4 py-2.5 font-medium text-right">Tokens</th>
                  <th className="px-4 py-2.5 font-medium text-right">Billing</th>
                </tr>
              </thead>
              <tbody>
                {modelBreakdown.map((row) => (
                  <tr key={row.model} className="border-b border-line last:border-b-0">
                    <td className="px-4 py-3 font-mono text-xs text-fg-2">{row.model}</td>
                    <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                      {row.stages}
                    </td>
                    <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                      {formatTokens(row.inputTokens)} <span className="text-fg-muted">/</span>{" "}
                      {formatTokens(row.outputTokens)}
                    </td>
                    <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">
                      {formatUsdMicros(row.totalUsdMicros)}
                    </td>
                  </tr>
                ))}
              </tbody>
              <tfoot>
                <tr className="border-t border-line-strong bg-overlay">
                  <td className="px-4 py-3 font-medium text-fg">Total</td>
                  <td className="px-4 py-3 text-right font-mono text-xs tabular-nums font-medium text-fg">
                    {modelStageCount}
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs tabular-nums font-medium text-fg">
                    {formatTokens(totalInput)} <span className="text-fg-muted">/</span>{" "}
                    {formatTokens(totalOutput)}
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">
                    {formatUsdMicros(totalUsdMicros)}
                  </td>
                </tr>
              </tfoot>
            </table>
          </div>
        </div>
      ) : null}
    </div>
  );
}
