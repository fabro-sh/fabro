import { useCallback, useEffect, useRef, useState } from "react";

export interface QueryResult {
  columns: string[];
  rows: Array<Record<string, string | number>>;
  elapsed: number;
  rowsRead: number;
  bytesRead: number;
  rowsReturned: number;
}

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

/**
 * Synchronizes the mock Insights query runner with the browser timer queue.
 * Starting a new run clears any pending timer, and the active timer is cleared
 * on unmount so stale completions cannot update React state.
 */
export function useInsightsQueryRunner(initialSql: string) {
  const [result, setResult] = useState<QueryResult | null>(() =>
    generateMockResult(initialSql),
  );
  const [isRunning, setIsRunning] = useState(false);
  const runRequestIdRef = useRef(0);
  const runTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearPendingRun = useCallback(() => {
    if (runTimeoutRef.current !== null) {
      clearTimeout(runTimeoutRef.current);
      runTimeoutRef.current = null;
    }
  }, []);

  const runQuery = useCallback((sql: string) => {
    const requestId = runRequestIdRef.current + 1;
    runRequestIdRef.current = requestId;
    clearPendingRun();
    setIsRunning(true);
    const delay = 200 + Math.random() * 400;
    runTimeoutRef.current = setTimeout(() => {
      if (runRequestIdRef.current !== requestId) return;
      runTimeoutRef.current = null;
      setResult(generateMockResult(sql));
      setIsRunning(false);
    }, delay);
  }, [clearPendingRun]);

  useEffect(() => {
    return () => {
      runRequestIdRef.current += 1;
      clearPendingRun();
    };
  }, [clearPendingRun]);

  return { result, isRunning, runQuery };
}
