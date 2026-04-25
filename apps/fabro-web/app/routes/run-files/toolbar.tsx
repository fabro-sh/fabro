import type { RefObject } from "react";
import { ArrowPathIcon } from "@heroicons/react/20/solid";

/**
 * Internal value used by `@pierre/diffs`. The UI labels "unified" as
 * "Stacked" to match the upstream library's branding (see diffs.com), so
 * the on-screen label and the stored value intentionally diverge.
 */
export type DiffStyle = "split" | "unified";

export function Toolbar({
  totalChanged,
  onRefresh,
  refreshing,
  refreshDisabled,
  freshness,
  refreshButtonRef,
  diffStyle,
  onDiffStyleChange,
  diffStyleForced,
}: {
  /** From `meta.total_changed`. May exceed the rendered file list when truncated. */
  totalChanged: number;
  onRefresh: () => void;
  refreshing: boolean;
  /** True when the server has nothing new to show (to_sha unchanged). */
  refreshDisabled: boolean;
  freshness: string | null;
  refreshButtonRef?: RefObject<HTMLButtonElement | null>;
  diffStyle: DiffStyle;
  onDiffStyleChange: (style: DiffStyle) => void;
  /**
   * True when the md breakpoint has forced unified view — the toggle
   * reflects the forced state but saving it would stomp the user's
   * desktop preference, so the parent keeps persistence off while
   * `diffStyleForced` is true.
   */
  diffStyleForced: boolean;
}) {
  const refreshTitle = refreshing
    ? "Refreshing"
    : refreshDisabled
      ? "Up to date"
      : "Refresh";
  return (
    <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-line pb-3">
      <p className="text-base font-semibold text-fg">
        <span className="tabular-nums">{totalChanged}</span>
        {" "}
        {totalChanged === 1 ? "file" : "files"} changed
      </p>
      <div className="flex items-center gap-3 text-xs">
        {freshness ? (
          <span
            aria-live="polite"
            className="hidden min-w-0 truncate text-fg-muted md:inline"
          >
            {freshness}
          </span>
        ) : null}
        <DiffLayoutToggle
          value={diffStyle}
          onChange={onDiffStyleChange}
          forced={diffStyleForced}
        />
        <button
          ref={refreshButtonRef}
          type="button"
          onClick={onRefresh}
          disabled={refreshing || refreshDisabled}
          aria-label={refreshing ? "Refreshing files" : "Refresh files"}
          title={refreshTitle}
          className="relative inline-flex size-7 items-center justify-center rounded-md border border-line bg-panel text-fg-3 transition-colors hover:bg-overlay hover:text-fg disabled:cursor-default disabled:opacity-60 disabled:hover:bg-panel disabled:hover:text-fg-3"
        >
          <ArrowPathIcon
            className={`size-3.5 ${refreshing ? "animate-spin" : ""}`}
            aria-hidden="true"
          />
          <span
            className="pointer-fine:hidden absolute top-1/2 left-1/2 size-[max(100%,3rem)] -translate-x-1/2 -translate-y-1/2"
            aria-hidden="true"
          />
        </button>
      </div>
    </div>
  );
}

function DiffLayoutToggle({
  value,
  onChange,
  forced,
}: {
  value: DiffStyle;
  onChange: (style: DiffStyle) => void;
  forced: boolean;
}) {
  const btn =
    "rounded px-2.5 py-1 text-xs font-medium transition-colors disabled:opacity-60";
  const active = "bg-overlay-strong text-fg";
  const inactive = "text-fg-3 hover:text-fg";
  return (
    <div
      className="inline-flex rounded-md bg-panel-alt p-0.5 ring-1 ring-line"
      role="group"
      aria-label="Diff layout"
    >
      <button
        type="button"
        onClick={() => onChange("split")}
        disabled={forced}
        aria-pressed={value === "split"}
        className={`${btn} ${value === "split" ? active : inactive}`}
      >
        Split
      </button>
      <button
        type="button"
        onClick={() => onChange("unified")}
        disabled={forced}
        aria-pressed={value === "unified"}
        className={`${btn} ${value === "unified" ? active : inactive}`}
      >
        Stacked
      </button>
    </div>
  );
}
