import { apiJson } from "../api";
import { formatDurationSecs } from "../lib/format";
import type { RunUsage } from "@qltysh/fabro-api-client";

export async function loader({ request, params }: any) {
  const usage = await apiJson<RunUsage>(`/runs/${params.id}/usage`, { request });
  const stages = usage.stages.map((s) => ({
    stage: s.stage.name,
    model: s.model.id,
    inputTokens: s.usage.input_tokens,
    outputTokens: s.usage.output_tokens,
    runtime: formatDurationSecs(s.runtime_secs),
    cost: s.usage.cost,
  }));
  const totalRuntime = formatDurationSecs(usage.totals.runtime_secs);
  const totalCost = usage.totals.cost;
  const totalInput = usage.totals.input_tokens;
  const totalOutput = usage.totals.output_tokens;
  const modelBreakdown = usage.by_model
    .map((m) => ({
      model: m.model.id,
      stages: m.stages,
      inputTokens: m.usage.input_tokens,
      outputTokens: m.usage.output_tokens,
      cost: m.usage.cost,
    }))
    .sort((a, b) => b.cost - a.cost);
  return { stages, totalRuntime, totalCost, totalInput, totalOutput, modelBreakdown };
}

function formatTokens(n: number) {
  return `${(n / 1000).toFixed(1)}k`;
}

export default function RunUsage({ loaderData }: any) {
  const { stages, totalRuntime, totalCost, totalInput, totalOutput, modelBreakdown } = loaderData;
  return (
    <div className="space-y-6">
      <div className="rounded-md border border-line overflow-hidden">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-line bg-panel/60 text-left text-xs text-fg-muted">
              <th className="px-4 py-2.5 font-medium">Stage</th>
              <th className="px-4 py-2.5 font-medium">Model</th>
              <th className="px-4 py-2.5 font-medium text-right">Tokens</th>
              <th className="px-4 py-2.5 font-medium text-right">Run time</th>
              <th className="px-4 py-2.5 font-medium text-right">Cost</th>
            </tr>
          </thead>
          <tbody>
            {stages.map((row) => (
              <tr key={row.stage} className="border-b border-line last:border-b-0">
                <td className="px-4 py-3 text-fg-2">{row.stage}</td>
                <td className="px-4 py-3 font-mono text-xs text-fg-3">{row.model}</td>
                <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                  {formatTokens(row.inputTokens)} <span className="text-fg-muted">/</span> {formatTokens(row.outputTokens)}
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">{row.runtime}</td>
                <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">${row.cost.toFixed(2)}</td>
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr className="border-t border-line-strong bg-panel/40">
              <td className="px-4 py-3 font-medium text-fg">Total</td>
              <td />
              <td className="px-4 py-3 text-right font-mono text-xs tabular-nums font-medium text-fg">
                {formatTokens(totalInput)} <span className="text-fg-muted">/</span> {formatTokens(totalOutput)}
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">{totalRuntime}</td>
              <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">${totalCost.toFixed(2)}</td>
            </tr>
          </tfoot>
        </table>
      </div>

      <div>
        <h3 className="mb-3 text-xs font-medium uppercase tracking-wider text-fg-muted">By Model</h3>
        <div className="rounded-md border border-line overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-line bg-panel/60 text-left text-xs text-fg-muted">
                <th className="px-4 py-2.5 font-medium">Model</th>
                <th className="px-4 py-2.5 font-medium text-right">Stages</th>
                <th className="px-4 py-2.5 font-medium text-right">Tokens</th>
                <th className="px-4 py-2.5 font-medium text-right">Cost</th>
              </tr>
            </thead>
            <tbody>
              {modelBreakdown.map((row) => (
                <tr key={row.model} className="border-b border-line last:border-b-0">
                  <td className="px-4 py-3 font-mono text-xs text-fg-2">{row.model}</td>
                  <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">{row.stages}</td>
                  <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                    {formatTokens(row.inputTokens)} <span className="text-fg-muted">/</span> {formatTokens(row.outputTokens)}
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs text-fg-3">${row.cost.toFixed(2)}</td>
                </tr>
              ))}
            </tbody>
            <tfoot>
              <tr className="border-t border-line-strong bg-panel/40">
                <td className="px-4 py-3 font-medium text-fg">Total</td>
                <td className="px-4 py-3 text-right font-mono text-xs tabular-nums font-medium text-fg">{stages.length}</td>
                <td className="px-4 py-3 text-right font-mono text-xs tabular-nums font-medium text-fg">
                  {formatTokens(totalInput)} <span className="text-fg-muted">/</span> {formatTokens(totalOutput)}
                </td>
                <td className="px-4 py-3 text-right font-mono text-xs font-medium text-fg">${totalCost.toFixed(2)}</td>
              </tr>
            </tfoot>
          </table>
        </div>
      </div>
    </div>
  );
}
