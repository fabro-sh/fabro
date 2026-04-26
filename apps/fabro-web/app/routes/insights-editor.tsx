import { useState, useRef, useEffect, useCallback } from "react";
import { useLocation } from "react-router";
import {
  Dialog,
  DialogPanel,
  DialogTitle,
} from "@headlessui/react";
import {
  PlayIcon,
  BookmarkIcon,
  SparklesIcon,
  TableCellsIcon,
  ChartBarIcon,
  XMarkIcon,
  ArrowPathIcon,
  PencilIcon,
} from "@heroicons/react/24/outline";
import { formatBytes } from "../lib/format";

// ── Types ──

interface QueryResult {
  columns: string[];
  rows: Array<Record<string, string | number>>;
  elapsed: number;
  rowsRead: number;
  bytesRead: number;
  rowsReturned: number;
}

type ResultView = "chart" | "table";

// ── Mock data ──

function generateMockResult(sql: string): QueryResult {
  const lowerSql = sql.toLowerCase();

  if (lowerSql.includes("workflow_name") && lowerSql.includes("avg")) {
    return {
      columns: ["workflow_name", "avg_duration", "run_count"],
      rows: [
        { workflow_name: "Expand Product", avg_duration: 342.5, run_count: 48 },
        { workflow_name: "Implement Feature", avg_duration: 287.3, run_count: 156 },
        { workflow_name: "Security Scan", avg_duration: 198.1, run_count: 312 },
        { workflow_name: "Fix Build", avg_duration: 145.7, run_count: 482 },
        { workflow_name: "Sync Drift", avg_duration: 89.2, run_count: 94 },
        { workflow_name: "Dependency Audit", avg_duration: 67.4, run_count: 201 },
      ],
      elapsed: 0.531,
      rowsRead: 5182366,
      bytesRead: 357780000,
      rowsReturned: 6,
    };
  }

  if (lowerSql.includes("failure_rate") || lowerSql.includes("failed")) {
    return {
      columns: ["day", "failures", "total", "failure_rate"],
      rows: Array.from({ length: 14 }, (_, i) => {
        const d = new Date();
        d.setDate(d.getDate() - i);
        const total = 80 + Math.floor(Math.random() * 60);
        const failures = Math.floor(Math.random() * 15);
        return {
          day: d.toISOString().slice(0, 10),
          failures,
          total,
          failure_rate: Math.round((1000 * failures) / total) / 10,
        };
      }),
      elapsed: 0.287,
      rowsRead: 2841092,
      bytesRead: 198400000,
      rowsReturned: 14,
    };
  }

  return {
    columns: ["repo", "runs", "total_additions", "total_deletions"],
    rows: [
      { repo: "fabro-engine", runs: 482, total_additions: 28450, total_deletions: 12300 },
      { repo: "fabro-web", runs: 356, total_additions: 19200, total_deletions: 8900 },
      { repo: "fabro-cli", runs: 198, total_additions: 8700, total_deletions: 4200 },
      { repo: "fabro-docs", runs: 145, total_additions: 12100, total_deletions: 3400 },
      { repo: "fabro-sdk", runs: 89, total_additions: 5600, total_deletions: 2100 },
      { repo: "fabro-infra", runs: 67, total_additions: 3200, total_deletions: 1800 },
      { repo: "fabro-actions", runs: 42, total_additions: 2100, total_deletions: 980 },
      { repo: "fabro-proto", runs: 28, total_additions: 1400, total_deletions: 650 },
    ],
    elapsed: 0.148,
    rowsRead: 1204588,
    bytesRead: 89200000,
    rowsReturned: 8,
  };
}

// ── Formatting helpers ──

function formatNumber(n: number): string {
  return n.toLocaleString();
}

// ── Chart component ──

const BAR_COLORS = [
  "rgba(90, 200, 168, 0.85)",
  "rgba(103, 178, 215, 0.85)",
  "rgba(181, 221, 239, 0.65)",
  "rgba(240, 164, 91, 0.75)",
];

function BarChart({ result }: { result: QueryResult }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(0);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) {
        setContainerWidth(entry.contentRect.width);
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const labelCol = result.columns[0];
  const valueCols = result.columns.slice(1).filter((col) => {
    const firstVal = result.rows[0]?.[col];
    return typeof firstVal === "number";
  });

  if (valueCols.length === 0 || result.rows.length === 0) {
    return (
      <div className="flex h-64 items-center justify-center text-sm text-fg-muted">
        No numeric columns to chart
      </div>
    );
  }

  const valueCol = valueCols[0];
  const maxVal = Math.max(...result.rows.map((r) => {
    const v = r[valueCol];
    return typeof v === "number" ? v : 0;
  }));

  const chartHeight = 260;
  const yAxisWidth = 52;
  const padding = { top: 12, bottom: 48, right: 16 };
  const plotHeight = chartHeight - padding.top - padding.bottom;
  const plotWidth = containerWidth - yAxisWidth - padding.right;
  const barCount = result.rows.length;
  const gap = Math.max(8, Math.min(16, plotWidth / barCount * 0.3));
  const barWidth = Math.max(12, (plotWidth - gap * (barCount + 1)) / barCount);

  const tickCount = 5;
  const yTicks = Array.from({ length: tickCount }, (_, i) =>
    Math.round((maxVal * (tickCount - 1 - i)) / (tickCount - 1)),
  );

  return (
    <div ref={containerRef}>
      {containerWidth > 0 && (
        <svg
          width={containerWidth}
          height={chartHeight}
          className="select-none"
        >
          {/* Y-axis gridlines + labels */}
          {yTicks.map((tick, i) => {
            const y = padding.top + (plotHeight * i) / (tickCount - 1);
            return (
              <g key={tick}>
                <line
                  x1={yAxisWidth}
                  y1={y}
                  x2={containerWidth - padding.right}
                  y2={y}
                  stroke="var(--chart-gridline)"
                  strokeDasharray="4,4"
                />
                <text
                  x={yAxisWidth - 8}
                  y={y + 4}
                  textAnchor="end"
                  fill="var(--chart-axis-label)"
                  fontSize="11"
                  fontFamily="JetBrains Mono, monospace"
                >
                  {tick >= 1000 ? `${(tick / 1000).toFixed(tick >= 10000 ? 0 : 1)}k` : tick}
                </text>
              </g>
            );
          })}

          {/* Bars */}
          {result.rows.map((row, i) => {
            const val = row[valueCol];
            const numVal = typeof val === "number" ? val : 0;
            const barHeight = maxVal > 0 ? (numVal / maxVal) * plotHeight : 0;
            const x = yAxisWidth + gap + i * (barWidth + gap);
            const y = padding.top + plotHeight - barHeight;
            const label = String(row[labelCol]);
            const colorIndex = i % BAR_COLORS.length;
            const maxLabelLen = Math.floor(barWidth / 6);

            return (
              <g key={label + i}>
                <rect
                  x={x}
                  y={y}
                  width={barWidth}
                  height={barHeight}
                  rx={3}
                  fill={BAR_COLORS[colorIndex]}
                  className="transition-opacity hover:opacity-100"
                  opacity={0.85}
                />
                <title>
                  {label}: {formatNumber(numVal)}
                </title>
                <text
                  x={x + barWidth / 2}
                  y={chartHeight - padding.bottom + 16}
                  textAnchor="middle"
                  fill="var(--chart-axis-label)"
                  fontSize="10"
                  fontFamily="JetBrains Mono, monospace"
                >
                  {label.length > maxLabelLen
                    ? label.slice(0, Math.max(3, maxLabelLen - 1)) + "\u2026"
                    : label}
                </text>
              </g>
            );
          })}
        </svg>
      )}
      <div className="mt-1 flex items-center gap-4 px-2">
        <span className="font-mono text-[10px] uppercase tracking-wider text-fg-muted">
          {valueCol.replace(/_/g, " ")}
        </span>
      </div>
    </div>
  );
}

// ── Table component ──

function ResultTable({ result }: { result: QueryResult }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-left">
        <thead>
          <tr className="border-b border-line">
            {result.columns.map((col) => (
              <th
                key={col}
                className="whitespace-nowrap px-3 py-2.5 font-mono text-[11px] font-semibold uppercase tracking-wider text-fg-muted"
              >
                {col}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {result.rows.map((row) => (
            <tr
              key={String(row[result.columns[0]])}
              className="border-b border-line transition-colors hover:bg-overlay"
            >
              {result.columns.map((col) => {
                const val = row[col];
                const isNum = typeof val === "number";
                return (
                  <td
                    key={col}
                    className={`whitespace-nowrap px-3 py-2 font-mono text-xs ${
                      isNum ? "tabular-nums text-fg-2" : "text-fg-3"
                    }`}
                  >
                    {isNum ? formatNumber(val) : String(val)}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ── SQL Editor with line numbers ──

function SqlEditor({
  value,
  onChange,
  onRun,
}: {
  value: string;
  onChange: (v: string) => void;
  onRun: () => void;
}) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const lineNumbersRef = useRef<HTMLDivElement>(null);
  const lineCount = value.split("\n").length;

  const syncScroll = useCallback(() => {
    if (textareaRef.current && lineNumbersRef.current) {
      lineNumbersRef.current.scrollTop = textareaRef.current.scrollTop;
    }
  }, []);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Ctrl/Cmd + Enter to run
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      onRun();
      return;
    }
    // Tab inserts spaces
    if (e.key === "Tab") {
      e.preventDefault();
      const textarea = e.currentTarget;
      const start = textarea.selectionStart;
      const end = textarea.selectionEnd;
      const newValue = value.slice(0, start) + "  " + value.slice(end);
      onChange(newValue);
      requestAnimationFrame(() => {
        textarea.selectionStart = start + 2;
        textarea.selectionEnd = start + 2;
      });
    }
  };

  return (
    <div className="relative flex overflow-hidden rounded-md border border-line bg-page/80 font-mono text-sm">
      {/* Line numbers */}
      <div
        ref={lineNumbersRef}
        className="pointer-events-none flex shrink-0 flex-col overflow-hidden border-r border-line bg-page/60 px-3 py-3 text-right leading-[1.625rem] text-fg-muted select-none"
        aria-hidden="true"
      >
        {Array.from({ length: lineCount }, (_, i) => (
          <span key={i + 1} className="text-[11px]">
            {i + 1}
          </span>
        ))}
      </div>
      {/* Textarea */}
      <textarea
        ref={textareaRef}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onScroll={syncScroll}
        onKeyDown={handleKeyDown}
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
        className="min-h-[7lh] w-full resize-y bg-transparent px-4 py-3 leading-[1.625rem] text-fg-2 placeholder-fg-muted outline-none"
        placeholder="SELECT * FROM runs LIMIT 10"
      />
    </div>
  );
}

// ── Main page ──

const DEFAULT_SQL =
  "SELECT workflow_name, AVG(duration_seconds) as avg_duration,\n       COUNT(*) as run_count\nFROM runs\nGROUP BY workflow_name\nORDER BY avg_duration DESC\nLIMIT 20";

export default function InsightsEditor() {
  const location = useLocation();
  const navState = location.state as { sql?: string; name?: string } | null;

  const [sql, setSql] = useState(navState?.sql ?? DEFAULT_SQL);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [resultView, setResultView] = useState<ResultView>("chart");
  const [isRunning, setIsRunning] = useState(false);
  const [queryName, setQueryName] = useState(navState?.name ?? "Run duration by workflow");
  const [isEditingName, setIsEditingName] = useState(false);
  const nameInputRef = useRef<HTMLInputElement>(null);
  const [showAiDialog, setShowAiDialog] = useState(false);
  const [aiPrompt, setAiPrompt] = useState("");

  const runQuery = useCallback(() => {
    setIsRunning(true);
    const delay = 200 + Math.random() * 400;
    setTimeout(() => {
      setResult(generateMockResult(sql));
      setIsRunning(false);
    }, delay);
  }, [sql]);

  // Load query from navigation state
  useEffect(() => {
    if (navState?.sql) {
      setSql(navState.sql);
      if (navState.name) {
        setQueryName(navState.name);
      }
    }
  }, [navState]);

  // Run default query on mount
  const hasRun = useRef(false);
  useEffect(() => {
    if (!hasRun.current) {
      hasRun.current = true;
      runQuery();
    }
  }, [runQuery]);

  return (
    <div className="space-y-4">
      {/* ── Toolbar + Editor ── */}
      <div>
        {/* Toolbar */}
        <div className="flex items-center gap-2 pb-3">
          {/* Query name */}
          {isEditingName ? (
            <input
              ref={nameInputRef}
              type="text"
              value={queryName}
              onChange={(e) => setQueryName(e.target.value)}
              onBlur={() => setIsEditingName(false)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === "Escape") {
                  setIsEditingName(false);
                }
              }}
              placeholder="Untitled query"
              className="min-w-0 max-w-xs rounded border border-focus bg-page/60 px-2 py-0.5 text-sm font-medium text-fg-2 placeholder-fg-muted outline-none"
            />
          ) : (
            <div className="flex items-center gap-1.5">
              <span className="text-sm font-medium text-fg-2">
                {queryName || "Untitled query"}
              </span>
              <button
                type="button"
                onClick={() => {
                  setIsEditingName(true);
                  requestAnimationFrame(() => nameInputRef.current?.select());
                }}
                className="rounded p-0.5 text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3"
              >
                <PencilIcon className="size-3.5" />
              </button>
            </div>
          )}

          {/* Push buttons to the right */}
          <div className="ml-auto" />

          {/* SQL AI */}
          <button
            type="button"
            onClick={() => setShowAiDialog(true)}
            className="inline-flex items-center gap-1.5 rounded-md border border-line px-3 py-1.5 text-sm font-medium text-fg-3 transition-colors hover:border-teal-500/30 hover:bg-overlay hover:text-fg"
          >
            <SparklesIcon className="size-3.5 text-teal-500" />
            SQL AI
          </button>

          {/* Save */}
          <button
            type="button"
            className="inline-flex items-center gap-1.5 rounded-md border border-line px-3 py-1.5 text-sm font-medium text-fg-3 transition-colors hover:border-line-strong hover:bg-overlay hover:text-fg"
          >
            <BookmarkIcon className="size-3.5" />
            Save
          </button>

          {/* Run */}
          <button
            type="button"
            onClick={runQuery}
            disabled={isRunning || sql.trim().length === 0}
            className="inline-flex items-center gap-1.5 rounded-md border border-mint/20 bg-mint/5 px-3.5 py-1.5 text-sm font-medium text-mint transition-all hover:border-mint/50 hover:bg-mint/10 hover:text-fg disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-mint/20 disabled:hover:bg-mint/5 disabled:hover:text-mint"
          >
            {isRunning ? (
              <ArrowPathIcon className="size-3.5 animate-spin" />
            ) : (
              <PlayIcon className="size-3.5" />
            )}
            {isRunning ? "Running\u2026" : "Run"}
            <kbd className="ml-1 hidden rounded border border-line-strong bg-overlay px-1 py-0.5 font-sans text-[10px] leading-none text-fg-muted sm:inline">
              {"\u2318\u21B5"}
            </kbd>
          </button>
        </div>

        <SqlEditor value={sql} onChange={setSql} onRun={runQuery} />
      </div>

      {/* ── Results bar + content ── */}
      {result && (
        <>
          {/* Results bar */}
          <div className="flex items-center justify-between">
            {/* Query stats */}
            <div className="flex items-center gap-5 font-mono text-[11px] tabular-nums text-fg-muted">
              <span>
                Elapsed:{" "}
                <span className="text-fg-3">
                  {result.elapsed.toFixed(3)}s
                </span>
              </span>
              <span>
                Read:{" "}
                <span className="text-fg-3">
                  {formatNumber(result.rowsRead)} rows
                </span>{" "}
                ({formatBytes(result.bytesRead)})
              </span>
              <span>
                Returned:{" "}
                <span className="text-fg-3">
                  {formatNumber(result.rowsReturned)} rows
                </span>
              </span>
            </div>

            {/* View toggle */}
            <div className="flex items-center gap-1 rounded-md border border-line bg-panel/80 p-0.5">
              <button
                type="button"
                onClick={() => setResultView("chart")}
                className={`inline-flex items-center gap-1.5 rounded px-2.5 py-1 text-xs font-medium transition-colors ${
                  resultView === "chart"
                    ? "bg-overlay text-teal-500"
                    : "text-fg-muted hover:text-fg-3"
                }`}
              >
                <ChartBarIcon className="size-3.5" />
                Chart
              </button>
              <button
                type="button"
                onClick={() => setResultView("table")}
                className={`inline-flex items-center gap-1.5 rounded px-2.5 py-1 text-xs font-medium transition-colors ${
                  resultView === "table"
                    ? "bg-overlay text-teal-500"
                    : "text-fg-muted hover:text-fg-3"
                }`}
              >
                <TableCellsIcon className="size-3.5" />
                Table
              </button>
            </div>
          </div>

          {/* Results content */}
          <div className="rounded-md border border-line bg-panel/60 p-4">
            {resultView === "chart" ? (
              <BarChart result={result} />
            ) : (
              <ResultTable result={result} />
            )}
          </div>
        </>
      )}

      {/* ── Running overlay ── */}
      {isRunning && !result && (
        <div className="flex h-48 items-center justify-center rounded-md border border-line bg-panel/60">
          <div className="flex items-center gap-3 text-sm text-fg-muted">
            <ArrowPathIcon className="size-5 animate-spin text-teal-500" />
            Executing query&hellip;
          </div>
        </div>
      )}

      {/* ── AI Dialog ── */}
      <Dialog
        open={showAiDialog}
        onClose={() => setShowAiDialog(false)}
        className="relative z-50"
      >
        <div
          className="fixed inset-0 bg-black/60 backdrop-blur-sm"
          aria-hidden="true"
        />
        <div className="fixed inset-0 flex items-start justify-center pt-[15vh]">
          <DialogPanel className="w-full max-w-lg rounded-lg border border-line-strong bg-panel shadow-2xl shadow-black/40">
            <div className="flex items-center justify-between border-b border-line px-5 py-3.5">
              <DialogTitle className="flex items-center gap-2 text-sm font-semibold text-fg">
                <SparklesIcon className="size-4 text-teal-500" />
                SQL AI
              </DialogTitle>
              <button
                type="button"
                onClick={() => setShowAiDialog(false)}
                className="text-fg-muted transition-colors hover:text-fg-3"
              >
                <XMarkIcon className="size-4" />
              </button>
            </div>
            <div className="p-5">
              <label htmlFor="ai-query-prompt" className="mb-2 block text-xs font-medium text-fg-3">
                Describe what you want to query
              </label>
              <textarea
                id="ai-query-prompt"
                value={aiPrompt}
                onChange={(e) => setAiPrompt(e.target.value)}
                placeholder="e.g. Show me the average build time per workflow over the last 30 days"
                className="w-full rounded-md border border-line bg-page/60 px-3 py-2.5 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus"
                rows={3}
              />
              <div className="mt-4 flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => setShowAiDialog(false)}
                  className="rounded-md border border-line px-3 py-1.5 text-sm text-fg-3 transition-colors hover:bg-overlay"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setSql(
                      "-- AI-generated query based on: " +
                        aiPrompt +
                        "\nSELECT workflow_name,\n       AVG(duration_seconds) as avg_duration,\n       COUNT(*) as run_count\nFROM runs\nWHERE created_at >= CURRENT_DATE - INTERVAL '30 days'\nGROUP BY workflow_name\nORDER BY avg_duration DESC",
                    );
                    setAiPrompt("");
                    setShowAiDialog(false);
                  }}
                  disabled={aiPrompt.trim().length === 0}
                  className="inline-flex items-center gap-1.5 rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-1.5 text-sm font-medium text-teal-300 transition-all hover:border-teal-500/50 hover:bg-teal-500/20 hover:text-fg disabled:cursor-not-allowed disabled:opacity-40"
                >
                  <SparklesIcon className="size-3.5" />
                  Generate SQL
                </button>
              </div>
            </div>
          </DialogPanel>
        </div>
      </Dialog>
    </div>
  );
}
