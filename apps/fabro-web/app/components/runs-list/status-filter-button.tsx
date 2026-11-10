import { CheckIcon, PlusCircleIcon } from "@heroicons/react/24/outline";
import { Listbox, ListboxButton, ListboxOption, ListboxOptions } from "@headlessui/react";
import type { BoardColumn } from "@qltysh/fabro-api-client";

import { columnStatusDisplay } from "../../data/runs";
import { TOOLBAR_BUTTON_CLASS } from "../ui";
import { STATUS_FILTER_OPTIONS } from "./preferences";

export function StatusFilterButton({
  value,
  onChange,
}: {
  value:    Set<BoardColumn>;
  onChange: (next: Set<BoardColumn>) => void;
}) {
  // Empty set and a full selection both mean "no filter is narrowing the
  // result set", so leave the button looking inactive in both cases.
  const active =
    value.size > 0 && value.size < STATUS_FILTER_OPTIONS.length;

  const selectedList = STATUS_FILTER_OPTIONS.filter((c) => value.has(c));
  const buttonLabel = (() => {
    if (!active) return "Status";
    if (selectedList.length === 1) {
      return `Status: ${columnStatusDisplay[selectedList[0]].label}`;
    }
    return `Status: ${selectedList.length}`;
  })();

  const selectedArray = [...value];

  return (
    <Listbox
      value={selectedArray}
      onChange={(next: BoardColumn[]) => onChange(new Set<BoardColumn>(next))}
      multiple
    >
      <ListboxButton className={TOOLBAR_BUTTON_CLASS} data-filtering={active || undefined}>
        <PlusCircleIcon className="size-4" aria-hidden="true" />
        <span>{buttonLabel}</span>
      </ListboxButton>
      <ListboxOptions
        anchor={{ to: "bottom start", gap: 6 }}
        className="menu z-20 focus:outline-none"
      >
        {STATUS_FILTER_OPTIONS.map((status) => {
          const display = columnStatusDisplay[status];
          return (
            <ListboxOption key={status} value={status} className="menu-item justify-between">
              {({ selected }) => (
                <>
                  <span className="flex items-center gap-2">
                    <span className={`size-2 shrink-0 rounded-full ${display.dot}`} aria-hidden="true" />
                    <span className={selected ? display.text : undefined}>{display.label}</span>
                  </span>
                  {selected ? (
                    <CheckIcon className="size-4 shrink-0 text-signal-ink" aria-hidden="true" />
                  ) : (
                    <span className="size-4 shrink-0" aria-hidden="true" />
                  )}
                </>
              )}
            </ListboxOption>
          );
        })}
      </ListboxOptions>
    </Listbox>
  );
}
