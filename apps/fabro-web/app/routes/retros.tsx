import { useState } from "react";
import { useNavigate } from "react-router";
import { MagnifyingGlassIcon, ChevronDownIcon } from "@heroicons/react/24/outline";
import { smoothnessConfig, formatDurationMs } from "../data/retros";
import type { SmoothnessRating } from "../data/retros";
import { apiJson } from "../api";
import type { PaginatedRetroList } from "@qltysh/fabro-api-client";

interface RetroRow {
  run_id: string;
  workflow_name: string;
  goal: string;
  timestamp: string;
  smoothness?: SmoothnessRating;
  total_duration_ms: number;
  friction_point_count: number;
}

export async function loader({ request }: any) {
  const { data: apiRetros } = await apiJson<PaginatedRetroList>("/retros", { request });
  const retros: RetroRow[] = apiRetros.map((r) => ({
    run_id: r.run.id,
    workflow_name: r.workflow.slug,
    goal: r.run.title,
    timestamp: r.timestamp,
    smoothness: r.smoothness as SmoothnessRating | undefined,
    total_duration_ms: r.stats.total_duration_ms,
    friction_point_count: r.friction_point_count,
  }));
  return { retros };
}

export function meta({}: any) {
  return [{ title: "Retros \u2014 Fabro" }];
}

const smoothnessOptions: Array<{ value: SmoothnessRating; label: string }> = [
  { value: "effortless", label: "Effortless" },
  { value: "smooth", label: "Smooth" },
  { value: "bumpy", label: "Bumpy" },
  { value: "struggled", label: "Struggled" },
  { value: "failed", label: "Failed" },
];

function SmoothnesssBadge({ smoothness }: { smoothness: SmoothnessRating | undefined }) {
  if (!smoothness) {
    return <span className="text-xs text-fg-muted">--</span>;
  }
  const config = smoothnessConfig[smoothness];
  return (
    <span className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ${config.bg} ${config.text}`}>
      <span className={`size-1.5 rounded-full ${config.dot}`} />
      {config.label}
    </span>
  );
}

function formatTimestamp(ts: string): string {
  const date = new Date(ts);
  return date.toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function truncate(text: string, maxLength: number): string {
  if (text.length <= maxLength) return text;
  return text.slice(0, maxLength) + "\u2026";
}

export default function Retros({ loaderData }: any) {
  const { retros } = loaderData;
  const navigate = useNavigate();
  const [query, setQuery] = useState("");
  const [smoothnessFilter, setSmoothnessFilter] = useState<SmoothnessRating | "all">("all");

  if (retros.length === 0) {
    return <p className="py-8 text-center text-sm text-fg-muted">No retrospectives yet.</p>;
  }

  const lowerQuery = query.toLowerCase();
  const filtered = retros.filter(
    (r) =>
      (smoothnessFilter === "all" || r.smoothness === smoothnessFilter) &&
      (r.goal.toLowerCase().includes(lowerQuery) ||
        r.workflow_name.toLowerCase().includes(lowerQuery)),
  );

  return (
    <div className="space-y-4">
      <div className="flex gap-3">
        <div className="relative flex-1">
          <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          <input
            type="text"
            placeholder="Search retros…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
          />
        </div>
        <div className="relative">
          <select
            value={smoothnessFilter}
            onChange={(e) => setSmoothnessFilter(e.target.value as SmoothnessRating | "all")}
            className="appearance-none rounded-md border border-line bg-panel/80 py-2 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0"
          >
            <option value="all">All smoothness</option>
            {smoothnessOptions.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
          <ChevronDownIcon className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
        </div>
      </div>

    <div className="rounded-md border border-line overflow-hidden">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-line bg-panel/60 text-left text-xs text-fg-muted">
            <th className="px-4 py-2.5 font-medium">Workflow</th>
            <th className="px-4 py-2.5 font-medium">Goal</th>
            <th className="px-4 py-2.5 font-medium">Smoothness</th>
            <th className="px-4 py-2.5 font-medium text-right">Duration</th>
            <th className="px-4 py-2.5 font-medium text-right">Frictions</th>
            <th className="px-4 py-2.5 font-medium text-right">When</th>
          </tr>
        </thead>
        <tbody>
          {filtered.map((retro) => (
            <tr key={retro.run_id} className="border-b border-line last:border-b-0 transition-colors hover:bg-overlay cursor-pointer" onClick={() => navigate(`/runs/${retro.run_id}/retro`)}>
              <td className="px-4 py-3 font-mono text-xs font-medium text-teal-500">
                {retro.workflow_name}
              </td>
              <td className="px-4 py-3 text-fg-2">
                {truncate(retro.goal, 60)}
              </td>
              <td className="px-4 py-3">
                <SmoothnesssBadge smoothness={retro.smoothness} />
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                {formatDurationMs(retro.total_duration_ms)}
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs tabular-nums text-fg-3">
                {retro.friction_point_count}
              </td>
              <td className="px-4 py-3 text-right font-mono text-xs text-fg-muted">
                {formatTimestamp(retro.timestamp)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
    </div>
  );
}
