import { CheckIcon, PlusCircleIcon } from "@heroicons/react/24/outline";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";

import { TOOLBAR_BUTTON_CLASS } from "../ui";

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
      <MenuButton className={TOOLBAR_BUTTON_CLASS} data-filtering={active || undefined}>
        <PlusCircleIcon className="size-4" aria-hidden="true" />
        <span>{active ? `${label}: ${activeLabel}` : label}</span>
      </MenuButton>
      <MenuItems
        anchor={{ to: "bottom start", gap: 6 }}
        className="menu scroll-pane z-20 max-h-72 focus:outline-none"
      >
        {options.map((option) => (
          <MenuItem key={option.value}>
            <button
              type="button"
              onClick={() => onChange(option.value)}
              className={`menu-item justify-between ${option.value === value ? "text-signal-ink" : ""}`}
            >
              <span className="truncate">{option.label}</span>
              {option.value === value && (
                <CheckIcon className="size-4 shrink-0" aria-hidden="true" />
              )}
            </button>
          </MenuItem>
        ))}
      </MenuItems>
    </Menu>
  );
}
