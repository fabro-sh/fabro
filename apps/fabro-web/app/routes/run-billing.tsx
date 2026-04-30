import { EmptyState } from "../components/state";
import { formatDurationSecs } from "../lib/format";
import { useRunBilling } from "../lib/queries";
import type { RunBilling } from "@qltysh/fabro-api-client";

function formatTokens(n: number) {
  return `${(n / 1000).toFixed(1)}k`;
}

function formatUsdMicros(usdMicros?: number | null) {
  return usdMicros == null ? "-" : `$${(usdMicros / 1_000_000).toFixed(2)}`;
}

function mapBilling(billing: RunBilling | undefined) {
  if (!billing) {
    return {
      stages: [],
      totalRuntime: formatDurationSecs(0),
      totalUsdMicros: undefined,
      totalInput: 0,
      totalOutput: 0,
      modelBreakdown: [],
    };
  }

  const stages = billing.stages.map((stage) => ({
    stage: stage.stage.name,
    model: stage.model.id,
    inputTokens: stage.billing.input_tokens,
    outputTokens: stage.billing.output_tokens + stage.billing.reasoning_tokens,
    runtime: formatDurationSecs(stage.runtime_secs),
    totalUsdMicros: stage.billing.total_usd_micros,
  }));
  const totalRuntime = formatDurationSecs(billing.totals.runtime_secs);
  const totalInput = billing.totals.input_tokens;
  const totalOutput = billing.totals.output_tokens + billing.totals.reasoning_tokens;
  const totalUsdMicros = billing.totals.total_usd_micros;
  const modelBreakdown = billing.by_model
    .map((entry) => ({
      model: entry.model.id,
      stages: entry.stages,
      inputTokens: entry.billing.input_tokens,
      outputTokens: entry.billing.output_tokens + entry.billing.reasoning_tokens,
      totalUsdMicros: entry.billing.total_usd_micros,
    }))
    .sort((a, b) => (b.totalUsdMicros ?? -1) - (a.totalUsdMicros ?? -1));
  return { stages, totalRuntime, totalUsdMicros, totalInput, totalOutput, modelBreakdown };
}

export default function RunBilling({ params }: { params: { id: string } }) {
  const billingQuery = useRunBilling(params.id);
  const loaderData = mapBilling(billingQuery.data);
  const { stages, totalRuntime, totalUsdMicros, totalInput, totalOutput, modelBreakdown } =
    loaderData;

  if (!stages.length) {
    return (
      <div className="py-12">
        <EmptyState
          title="No billing yet"
          description="Token usage and cost will appear here once stages complete."
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
            {stages.map((row) => (
              <tr key={row.stage} className="border-b border-line last:border-b-0">
                <td className="px-4 py-3 text-fg-2">{row.stage}</td>
                <td className="px-4 py-3 font-mono text-xs text-fg-3">{row.model}</td>
                <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                  {formatTokens(row.inputTokens)} <span className="text-fg-muted">/</span>{" "}
                  {formatTokens(row.outputTokens)}
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">{row.runtime}</td>
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
                {totalRuntime}
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">
                {formatUsdMicros(totalUsdMicros)}
              </td>
            </tr>
          </tfoot>
        </table>
      </div>

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
                  {stages.length}
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
    </div>
  );
}
