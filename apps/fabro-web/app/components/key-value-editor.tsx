import type { ReactNode } from "react";
import { PlusIcon, XMarkIcon } from "@heroicons/react/16/solid";

import { INPUT_CLASS } from "./ui";

export interface KeyValueEntry {
  key: string;
  value: string;
}

export function entriesFromMap(map: { [key: string]: string }): KeyValueEntry[] {
  return Object.entries(map).map(([key, value]) => ({ key, value }));
}

export function mapFromEntries(entries: KeyValueEntry[]): { [key: string]: string } {
  return Object.fromEntries(
    entries
      .map((entry): [string, string] => [entry.key.trim(), entry.value])
      .filter((entry) => entry[0] !== ""),
  );
}

export function KeyValueEditor({
  entries,
  onChange,
  keyPlaceholder,
  valuePlaceholder,
  addLabel,
  renderEntryHint,
}: {
  entries: KeyValueEntry[];
  onChange: (entries: KeyValueEntry[]) => void;
  keyPlaceholder: string;
  valuePlaceholder: string;
  addLabel: string;
  renderEntryHint?: (entry: KeyValueEntry, index: number) => ReactNode;
}) {
  function update(index: number, partial: Partial<KeyValueEntry>) {
    onChange(entries.map((entry, i) => (i === index ? { ...entry, ...partial } : entry)));
  }

  return (
    <div className="space-y-2">
      {entries.map((entry, index) => (
        <div key={index} className="space-y-1">
          <div className="flex items-center gap-2">
            <input
              type="text"
              aria-label="Key"
              value={entry.key}
              onChange={(e) => update(index, { key: e.target.value })}
              placeholder={keyPlaceholder}
              autoComplete="off"
              spellCheck={false}
              className={`${INPUT_CLASS} font-mono`}
            />
            <input
              type="text"
              aria-label="Value"
              value={entry.value}
              onChange={(e) => update(index, { value: e.target.value })}
              placeholder={valuePlaceholder}
              autoComplete="off"
              spellCheck={false}
              className={`${INPUT_CLASS} font-mono`}
            />
            <RemoveButton onClick={() => onChange(entries.filter((_, i) => i !== index))} />
          </div>
          {renderEntryHint ? renderEntryHint(entry, index) : null}
        </div>
      ))}
      <AddButton label={addLabel} onClick={() => onChange([...entries, { key: "", value: "" }])} />
    </div>
  );
}

function AddButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="inline-flex items-center gap-1.5 rounded-md border border-line bg-panel/80 px-2.5 py-1 text-xs font-medium text-fg-3 transition-colors hover:border-line-strong hover:bg-panel hover:text-fg"
    >
      <PlusIcon className="size-3.5" aria-hidden="true" />
      {label}
    </button>
  );
}

function RemoveButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Remove row"
      title="Remove"
      className="flex size-9 shrink-0 items-center justify-center rounded-lg text-fg-muted transition-colors hover:bg-overlay hover:text-coral"
    >
      <XMarkIcon className="size-4" aria-hidden="true" />
    </button>
  );
}
