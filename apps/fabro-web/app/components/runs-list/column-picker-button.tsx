import { AdjustmentsHorizontalIcon, CheckIcon } from "@heroicons/react/24/outline";
import { Listbox, ListboxButton, ListboxOption, ListboxOptions } from "@headlessui/react";

import { TOGGLEABLE_COLUMNS, toggleableColumnLabels } from "./toggleable-column";
import type { ToggleableColumn } from "./toggleable-column";

export function ColumnPickerButton({
  hidden,
  onChange,
}: {
  hidden:   Set<ToggleableColumn>;
  onChange: (next: Set<ToggleableColumn>) => void;
}) {
  const visible = TOGGLEABLE_COLUMNS.filter((col) => !hidden.has(col));
  return (
    <Listbox
      value={visible}
      onChange={(next: ToggleableColumn[]) => {
        const nextHidden = new Set<ToggleableColumn>(TOGGLEABLE_COLUMNS);
        for (const col of next) nextHidden.delete(col);
        onChange(nextHidden);
      }}
      multiple
    >
      <ListboxButton className="inline-flex items-center gap-1.5 rounded-md border border-line bg-panel/80 px-3 py-2 text-xs font-medium text-fg-muted transition-colors hover:text-fg-3">
        <AdjustmentsHorizontalIcon className="size-4" aria-hidden="true" />
        <span>View</span>
      </ListboxButton>
      <ListboxOptions
        anchor="bottom end"
        className="z-20 mt-1 min-w-[10rem] rounded-md border border-line bg-panel py-1 text-xs shadow-lg focus:outline-none"
      >
        {TOGGLEABLE_COLUMNS.map((col) => (
          <ListboxOption
            key={col}
            value={col}
            className={({ focus }) =>
              `flex cursor-pointer items-center justify-between gap-3 px-3 py-1.5 text-fg-2 ${focus ? "bg-overlay" : ""}`
            }
          >
            {({ selected }) => (
              <>
                <span>{toggleableColumnLabels[col]}</span>
                {selected ? (
                  <CheckIcon className="size-4 shrink-0 text-teal-500" aria-hidden="true" />
                ) : (
                  <span className="size-4 shrink-0" aria-hidden="true" />
                )}
              </>
            )}
          </ListboxOption>
        ))}
      </ListboxOptions>
    </Listbox>
  );
}
