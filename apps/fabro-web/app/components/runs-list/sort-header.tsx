import { ChevronDownIcon, ChevronUpDownIcon, ChevronUpIcon } from "@heroicons/react/24/outline";

export type SortDirection = "asc" | "desc";

export function SortHeader<TKey extends string>({
  label,
  sortKey,
  activeSort,
  direction,
  align = "left",
  onClick,
}: {
  label:      string;
  sortKey:    TKey;
  activeSort: TKey;
  direction:  SortDirection;
  align?:     "left" | "right";
  onClick:    (key: TKey) => void;
}) {
  const isActive = activeSort === sortKey;
  const ariaSort: "ascending" | "descending" | "none" = isActive
    ? direction === "asc"
      ? "ascending"
      : "descending"
    : "none";
  return (
    <th
      scope="col"
      aria-sort={ariaSort}
      className={`whitespace-nowrap px-3 py-2.5 font-medium ${align === "right" ? "text-right" : "text-left"}`}
    >
      <button
        type="button"
        onClick={() => onClick(sortKey)}
        className={`inline-flex items-center gap-1 transition-colors hover:text-fg-2 ${isActive ? "text-fg-2" : "text-fg-3"} ${align === "right" ? "ml-auto" : ""}`}
      >
        <span>{label}</span>
        {isActive ? (
          direction === "asc" ? (
            <ChevronUpIcon className="size-3.5 text-fg-3" aria-hidden="true" />
          ) : (
            <ChevronDownIcon className="size-3.5 text-fg-3" aria-hidden="true" />
          )
        ) : (
          <ChevronUpDownIcon className="size-3.5 text-fg-muted" aria-hidden="true" />
        )}
      </button>
    </th>
  );
}
