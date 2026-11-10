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
  align?:     "left" | "center" | "right";
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
      className={`whitespace-nowrap px-3 py-2 ${align === "right" ? "text-right" : align === "center" ? "text-center" : "text-left"}`}
    >
      <button
        type="button"
        onClick={() => onClick(sortKey)}
        className={`inline-flex items-center gap-1 font-bold transition-colors hover:text-fg ${isActive ? "text-fg" : "text-muted"} ${align === "right" ? "ml-auto" : align === "center" ? "mx-auto" : ""}`}
      >
        <span>{label}</span>
        {isActive ? (
          direction === "asc" ? (
            <ChevronUpIcon className="size-3.5 text-muted" aria-hidden="true" />
          ) : (
            <ChevronDownIcon className="size-3.5 text-muted" aria-hidden="true" />
          )
        ) : (
          <ChevronUpDownIcon className="size-3.5 text-muted" aria-hidden="true" />
        )}
      </button>
    </th>
  );
}
