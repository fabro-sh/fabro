import { AdjustmentsHorizontalIcon, CheckIcon } from "@heroicons/react/24/outline";
import { Listbox, ListboxButton, ListboxOption, ListboxOptions } from "@headlessui/react";

import { TOOLBAR_BUTTON_CLASS } from "../ui";
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
      <ListboxButton className={TOOLBAR_BUTTON_CLASS}>
        <AdjustmentsHorizontalIcon className="size-4" aria-hidden="true" />
        <span>View</span>
      </ListboxButton>
      <ListboxOptions
        anchor={{ to: "bottom end", gap: 6 }}
        className="menu z-20 focus:outline-none"
      >
        {TOGGLEABLE_COLUMNS.map((col) => (
          <ListboxOption key={col} value={col} className="menu-item justify-between">
            {({ selected }) => (
              <>
                <span>{toggleableColumnLabels[col]}</span>
                {selected ? (
                  <CheckIcon className="size-4 shrink-0 text-signal-ink" aria-hidden="true" />
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
