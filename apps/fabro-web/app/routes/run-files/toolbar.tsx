import type { RefObject } from "react";
import {
  Listbox,
  ListboxButton,
  ListboxOption,
  ListboxOptions,
} from "@headlessui/react";
import { ArrowPathIcon } from "@heroicons/react/20/solid";
import { CheckIcon, ChevronUpDownIcon } from "@heroicons/react/16/solid";
import type { RunFileScope } from "../../lib/query-keys";

/**
 * Internal value used by `@pierre/diffs`. The UI labels "unified" as
 * "Stacked" to match the upstream library's branding (see diffs.com), so
 * the on-screen label and the stored value intentionally diverge.
 */
export type DiffStyle = "split" | "unified";

type ChangeSummary = {
  totalChanged: number;
  additions: number;
  deletions: number;
};

export type DiffPickerValue =
  | { kind: "scope"; scope: RunFileScope }
  | { kind: "commit"; sha: string };

export type DiffCommitOption = {
  sha: string;
  label: string;
  title: string;
};

export function Toolbar({
  changeSummary,
  selection,
  commits,
  showScopePicker,
  onPickerChange,
  onRefresh,
  refreshing,
  refreshDisabled,
  freshness,
  refreshButtonRef,
  diffStyle,
  onDiffStyleChange,
  diffStyleForced,
}: {
  changeSummary: ChangeSummary;
  selection: DiffPickerValue;
  commits: DiffCommitOption[];
  showScopePicker: boolean;
  onPickerChange: (selection: DiffPickerValue) => void;
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
  const { totalChanged, additions, deletions } = changeSummary;
  const refreshTitle = refreshing
    ? "Refreshing"
    : refreshDisabled
      ? "Up to date"
      : "Refresh";
  return (
    <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-line pb-3">
      <div className="flex min-w-0 items-center gap-3">
        {showScopePicker ? (
          <DiffSelectionPicker
            value={selection}
            commits={commits}
            onChange={onPickerChange}
          />
        ) : null}
        <p className="text-base font-semibold text-fg">
          <span className="tabular-nums">{totalChanged}</span>
          {" "}
          {totalChanged === 1 ? "file" : "files"} changed
        </p>
        {totalChanged > 0 && (additions > 0 || deletions > 0) ? (
          <p className="font-mono text-sm tabular-nums">
            <span className="font-medium text-mint">+{additions}</span>
            <span className="ml-1 font-medium text-coral">−{deletions}</span>
          </p>
        ) : null}
      </div>
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
            className={`size-3.5 ${refreshing ? "animate-spin [animation-duration:450ms]" : ""}`}
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

const scopeOptions: Array<{ value: RunFileScope; label: string }> = [
  { value: "all", label: "All changes" },
  { value: "uncommitted", label: "Uncommitted" },
  { value: "committed", label: "Committed" },
];

function DiffSelectionPicker({
  value,
  commits,
  onChange,
}: {
  value: DiffPickerValue;
  commits: DiffCommitOption[];
  onChange: (selection: DiffPickerValue) => void;
}) {
  const selectedValue = pickerValueKey(value);
  const selectedLabel =
    value.kind === "scope"
      ? (scopeOptions.find((o) => o.value === value.scope) ?? scopeOptions[0])
          .label
      : (commits.find((commit) => commit.sha === value.sha)?.label ??
        value.sha.slice(0, 7));
  return (
    <Listbox value={selectedValue} onChange={(next) => onChange(parsePickerValue(next))}>
      <div className="relative">
        <ListboxButton
          aria-label="Diff selection"
          className="flex h-7 max-w-56 items-center gap-1.5 rounded-md border border-line bg-panel px-2.5 text-xs font-medium text-fg-3 transition-colors hover:bg-overlay hover:text-fg data-open:bg-overlay data-open:text-fg"
        >
          <span className="truncate">{selectedLabel}</span>
          <ChevronUpDownIcon className="size-3.5 text-fg-muted" aria-hidden="true" />
        </ListboxButton>
        <ListboxOptions
          transition
          anchor={{ to: "bottom start", gap: 4 }}
          className="z-20 w-64 origin-top-left rounded-md bg-panel py-1 shadow-xl shadow-black/30 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in focus:outline-none"
        >
          {scopeOptions.map((option) => (
            <ListboxOption
              key={option.value}
              value={`scope:${option.value}`}
              className="flex cursor-default items-center justify-between gap-3 px-3 py-1.5 text-xs text-fg-3 data-focus:bg-overlay data-focus:text-fg data-selected:text-fg"
            >
              {({ selected }) => (
                <>
                  <span className={selected ? "font-medium" : ""}>
                    {option.label}
                  </span>
                  {selected ? (
                    <CheckIcon
                      className="size-3.5 text-teal-300"
                      aria-hidden="true"
                    />
                  ) : null}
                </>
              )}
            </ListboxOption>
          ))}
          {commits.length > 0 ? (
            <hr className="my-1 border-t border-line" />
          ) : null}
          {commits.map((commit) => (
            <ListboxOption
              key={commit.sha}
              value={`commit:${commit.sha}`}
              title={commit.title}
              className="flex cursor-default items-center justify-between gap-3 px-3 py-1.5 text-xs text-fg-3 data-focus:bg-overlay data-focus:text-fg data-selected:text-fg"
            >
              {({ selected }) => (
                <>
                  <span className={`truncate ${selected ? "font-medium" : ""}`}>
                    {commit.label}
                  </span>
                  {selected ? (
                    <CheckIcon
                      className="size-3.5 shrink-0 text-teal-300"
                      aria-hidden="true"
                    />
                  ) : null}
                </>
              )}
            </ListboxOption>
          ))}
        </ListboxOptions>
      </div>
    </Listbox>
  );
}

function pickerValueKey(value: DiffPickerValue): string {
  return value.kind === "scope" ? `scope:${value.scope}` : `commit:${value.sha}`;
}

function parsePickerValue(value: string): DiffPickerValue {
  if (value.startsWith("commit:")) {
    return { kind: "commit", sha: value.slice("commit:".length) };
  }
  const scope = value.slice("scope:".length);
  if (scope === "all" || scope === "uncommitted" || scope === "committed") {
    return { kind: "scope", scope };
  }
  return { kind: "scope", scope: "committed" };
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
    <div className="inline-flex rounded-md bg-panel-alt p-0.5 ring-1 ring-line">
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
