import { Fragment, useMemo } from "react";

import { EmptyState } from "../components/state";
import { Tooltip } from "../components/ui";
import {
  formatDurationMs,
  formatTokenCount,
  formatUsdMicros,
} from "../lib/format";
import { useRunBilling } from "../lib/queries";
import { IN_FLIGHT_STAGE_STATES } from "../lib/stage-sidebar";
import { useTickingNow } from "../lib/time";
import type {
  BillingModelRef,
  RunBilling,
  RunBillingStage,
} from "@qltysh/fabro-api-client";

const EMPTY_VALUE = "—";

function formatTokens(n: number | null | undefined) {
  if (n == null) return EMPTY_VALUE;
  return formatTokenCount(n, { compactDecimal: true });
}

function formatUsdMicrosOrDash(usdMicros?: number | null): string {
  return formatUsdMicros(usdMicros) ?? EMPTY_VALUE;
}

function formatModelRef(model?: BillingModelRef | null): string | null {
  if (!model) return null;
  const speed = model.speed && model.speed !== "standard" ? ` · ${model.speed}` : "";
  return `${model.provider}:${model.model_id}${speed}`;
}

function isInFlight(stage: RunBillingStage): boolean {
  return stage.state != null && IN_FLIGHT_STAGE_STATES.has(stage.state);
}

function isVisibleRow(row: MappedStageRow): boolean {
  if (row.inFlight) return true;
  return (
    (row.inputTokens ?? 0) > 0 ||
    (row.outputTokens ?? 0) > 0 ||
    (row.totalUsdMicros ?? 0) > 0
  );
}

interface MappedStageRow {
  stage:            string;
  model:            string | null;
  inputTokens:      number | null;
  outputTokens:     number | null;
  cacheReadTokens:  number | null;
  cacheWriteTokens: number | null;
  wallTimeMs:       number;
  totalUsdMicros:   number | null | undefined;
  inFlight:         boolean;
}

function liveWallTimeMs(stage: RunBillingStage, now: number): number {
  if (stage.started_at) {
    const startedMs = new Date(stage.started_at).getTime();
    if (Number.isFinite(startedMs)) {
      return Math.max(0, now - startedMs);
    }
  }
  return stage.timing.wall_time_ms;
}

export const handle = { wide: true };

function mapStageRow(stage: RunBillingStage, wallTimeMs: number): MappedStageRow {
  const hasModel = stage.model != null;
  return {
    stage:            stage.stage.name,
    model:            formatModelRef(stage.model),
    inputTokens:      hasModel ? stage.billing.input_tokens : null,
    outputTokens:     hasModel
      ? stage.billing.output_tokens + stage.billing.reasoning_tokens
      : null,
    cacheReadTokens:  hasModel ? stage.billing.cache_read_tokens : null,
    cacheWriteTokens: hasModel ? stage.billing.cache_write_tokens : null,
    wallTimeMs,
    totalUsdMicros:   stage.billing.total_usd_micros,
    inFlight:         isInFlight(stage),
  };
}

/** Hover breakdown of the disjoint token buckets behind an `in / out` count. */
function TokenBreakdown({
  cacheReadTokens,
  cacheWriteTokens,
  inputTokens,
  outputTokens,
}: {
  cacheReadTokens:  number;
  cacheWriteTokens: number;
  inputTokens:      number;
  outputTokens:     number;
}) {
  const rows = [
    { label: "Cache read", value: cacheReadTokens },
    { label: "Cache creation", value: cacheWriteTokens },
    { label: "Uncached", value: inputTokens },
    { label: "Output", value: outputTokens },
  ];
  return (
    <div className="min-w-44 py-0.5">
      <div className="mb-1.5 border-b border-line pb-1 font-medium text-fg-2">
        Tokens in / out
      </div>
      <dl className="grid grid-cols-[1fr_auto] gap-x-6 gap-y-1">
        {rows.map((row) => (
          <Fragment key={row.label}>
            <dt className="text-fg-3">{row.label}</dt>
            <dd className="text-right font-mono tabular-nums text-fg">
              {formatTokens(row.value)}
            </dd>
          </Fragment>
        ))}
      </dl>
    </div>
  );
}

/**
 * Renders an `input / output` token count. When the row has model usage,
 * hovering the count reveals the cache breakdown.
 */
function TokensCell({
  inputTokens,
  outputTokens,
  cacheReadTokens,
  cacheWriteTokens,
}: {
  inputTokens:      number | null;
  outputTokens:     number | null;
  cacheReadTokens:  number | null;
  cacheWriteTokens: number | null;
}) {
  const display = (
    <>
      {formatTokens(inputTokens)} <span className="text-fg-muted">/</span>{" "}
      {formatTokens(outputTokens)}
    </>
  );
  if (
    inputTokens == null ||
    outputTokens == null ||
    cacheReadTokens == null ||
    cacheWriteTokens == null
  ) {
    return display;
  }
  return (
    <Tooltip
      label={
        <TokenBreakdown
          cacheReadTokens={cacheReadTokens}
          cacheWriteTokens={cacheWriteTokens}
          inputTokens={inputTokens}
          outputTokens={outputTokens}
        />
      }
    >
      <span>{display}</span>
    </Tooltip>
  );
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
    return billing.stages.map((stage) => mapStageRow(stage, stage.timing.wall_time_ms));
  }, [billing]);

  // The model breakdown is server-derived and stable across ticks too.
  const modelBreakdown = useMemo(() => {
    if (!billing) return [];
    return billing.by_model
      .map((entry) => ({
        model:            formatModelRef(entry.model) ?? EMPTY_VALUE,
        stages:           entry.stages,
        inputTokens:      entry.billing.input_tokens,
        outputTokens:     entry.billing.output_tokens + entry.billing.reasoning_tokens,
        cacheReadTokens:  entry.billing.cache_read_tokens,
        cacheWriteTokens: entry.billing.cache_write_tokens,
        totalUsdMicros:   entry.billing.total_usd_micros,
      }))
      .sort((a, b) => (b.totalUsdMicros ?? -1) - (a.totalUsdMicros ?? -1));
  }, [billing]);

  // Re-derive only the in-flight rows on each tick; everything else stays put.
  const rows = useMemo<MappedStageRow[]>(() => {
    if (!billing) return [];
    if (!hasInFlight) return completedRows;
    return billing.stages.map((stage, idx) =>
      isInFlight(stage)
        ? mapStageRow(stage, liveWallTimeMs(stage, now))
        : completedRows[idx],
    );
  }, [billing, completedRows, hasInFlight, now]);

  // While ticking, sum the displayed row runtimes so the footer updates in
  // lock-step. Otherwise trust the server's authoritative total.
  const totalWallTimeMs = hasInFlight
    ? rows.reduce((sum, row) => sum + row.wallTimeMs, 0)
    : (billing?.totals.timing.wall_time_ms ?? 0);

  const hasLlmStages = (billing?.by_model.length ?? 0) > 0;
  const totalInput = hasLlmStages ? (billing?.totals.input_tokens ?? null) : null;
  const totalOutput = hasLlmStages && billing
    ? billing.totals.output_tokens + billing.totals.reasoning_tokens
    : null;
  const totalCacheRead = hasLlmStages
    ? (billing?.totals.cache_read_tokens ?? null)
    : null;
  const totalCacheWrite = hasLlmStages
    ? (billing?.totals.cache_write_tokens ?? null)
    : null;
  const totalUsdMicros = billing?.totals.total_usd_micros;
  const modelStageCount = modelBreakdown.reduce((sum, row) => sum + row.stages, 0);
  const visibleRows = rows.filter(isVisibleRow);

  if (!visibleRows.length) {
    return (
      <div className="py-12">
        <EmptyState
          title={rows.length ? "No model usage" : "No stages yet"}
          description={
            rows.length
              ? "This run didn't call any AI models."
              : "Stages will appear as soon as the run starts executing."
          }
        />
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-5xl space-y-6">
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
            {visibleRows.map((row) => (
              <tr key={row.stage} className="border-b border-line last:border-b-0">
                <td className="px-4 py-3 text-fg-2">{row.stage}</td>
                <td className="px-4 py-3 font-mono text-xs text-fg-3">
                  {row.model ?? EMPTY_VALUE}
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                  <TokensCell
                    inputTokens={row.inputTokens}
                    outputTokens={row.outputTokens}
                    cacheReadTokens={row.cacheReadTokens}
                    cacheWriteTokens={row.cacheWriteTokens}
                  />
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">
                  {formatDurationMs(row.wallTimeMs)}
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">
                  {formatUsdMicrosOrDash(row.totalUsdMicros)}
                </td>
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr className="border-t border-line-strong bg-overlay">
              <td className="px-4 py-3 font-medium text-fg">Total</td>
              <td className="px-4 py-3 text-xs text-fg-muted">All models</td>
              <td className="px-4 py-3 text-right font-mono text-xs tabular-nums font-medium text-fg">
                <TokensCell
                  inputTokens={totalInput}
                  outputTokens={totalOutput}
                  cacheReadTokens={totalCacheRead}
                  cacheWriteTokens={totalCacheWrite}
                />
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">
                {formatDurationMs(totalWallTimeMs)}
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">
                {formatUsdMicrosOrDash(totalUsdMicros)}
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
                      <TokensCell
                        inputTokens={row.inputTokens}
                        outputTokens={row.outputTokens}
                        cacheReadTokens={row.cacheReadTokens}
                        cacheWriteTokens={row.cacheWriteTokens}
                      />
                    </td>
                    <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">
                      {formatUsdMicrosOrDash(row.totalUsdMicros)}
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
                    <TokensCell
                      inputTokens={totalInput}
                      outputTokens={totalOutput}
                      cacheReadTokens={totalCacheRead}
                      cacheWriteTokens={totalCacheWrite}
                    />
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">
                    {formatUsdMicrosOrDash(totalUsdMicros)}
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
