import { CheckIcon, PlusCircleIcon } from "@heroicons/react/24/outline";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";

export type FilterOption<T extends string> = { value: T; label: string };

export function FilterButton<T extends string>({
  label,
  value,
  allValue,
  options,
  onChange,
}: {
  label:    string;
  value:    T;
  allValue: T;
  options:  FilterOption<T>[];
  onChange: (next: T) => void;
}) {
  const active = value !== allValue;
  const activeLabel = options.find((opt) => opt.value === value)?.label;
  return (
    <Menu as="div" className="relative">
      <MenuButton
        className={`inline-flex items-center gap-1.5 rounded-md border px-3 py-2 text-xs font-medium transition-colors ${
          active
            ? "border-line-strong bg-panel text-fg-2"
            : "border-line bg-panel/80 text-fg-muted hover:text-fg-3"
        }`}
      >
        <PlusCircleIcon className="size-4" aria-hidden="true" />
        <span>{active ? `${label}: ${activeLabel}` : label}</span>
      </MenuButton>
      <MenuItems
        anchor="bottom start"
        className="z-20 mt-1 max-h-72 min-w-[12rem] overflow-y-auto rounded-md border border-line bg-panel py-1 text-xs shadow-lg focus:outline-none"
      >
        {options.map((option) => (
          <MenuItem key={option.value}>
            {({ focus }) => (
              <button
                type="button"
                onClick={() => onChange(option.value)}
                className={`flex w-full items-center justify-between gap-3 px-3 py-1.5 text-left ${
                  focus ? "bg-overlay" : ""
                } ${option.value === value ? "text-teal-500" : "text-fg-2"}`}
              >
                <span className="truncate">{option.label}</span>
                {option.value === value && (
                  <CheckIcon className="size-4 shrink-0" aria-hidden="true" />
                )}
              </button>
            )}
          </MenuItem>
        ))}
      </MenuItems>
    </Menu>
  );
}
