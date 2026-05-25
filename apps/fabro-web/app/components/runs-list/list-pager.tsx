import {
  ChevronDoubleLeftIcon,
  ChevronDoubleRightIcon,
  ChevronDownIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
} from "@heroicons/react/24/outline";

import { LIST_PAGE_SIZES } from "./preferences";

export function ListPager({
  page,
  pageSize,
  pageCount,
  hasMore,
  disabled,
  onPageChange,
  onPageSizeChange,
}: {
  page:             number;
  pageSize:         number;
  pageCount:        number | null;
  hasMore:          boolean;
  disabled:         boolean;
  onPageChange:     (page: number) => void;
  onPageSizeChange: (size: number) => void;
}) {
  const onFirstPage = page <= 1;
  const onLastPage = pageCount != null ? page >= pageCount : !hasMore;
  return (
    <nav
      aria-label="Pagination"
      className="flex items-center justify-between gap-6 pt-2 text-sm text-fg-3"
    >
      <div className="flex items-center gap-3">
        <label htmlFor="runs-page-size" className="text-fg-3">
          Rows per page
        </label>
        <div className="relative">
          <select
            id="runs-page-size"
            value={pageSize}
            onChange={(e) => onPageSizeChange(Number(e.target.value))}
            disabled={disabled}
            className="appearance-none rounded-md border border-line bg-panel/80 py-1.5 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0 disabled:opacity-60"
          >
            {LIST_PAGE_SIZES.map((size) => (
              <option key={size} value={size}>{size}</option>
            ))}
          </select>
          <ChevronDownIcon className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
        </div>
      </div>

      <span className="text-fg-3">
        Page {page}
        {pageCount != null ? <> of {pageCount}</> : null}
      </span>

      <div className="flex items-center gap-1.5">
        <PagerButton
          label="First page"
          onClick={() => onPageChange(1)}
          disabled={disabled || onFirstPage}
        >
          <ChevronDoubleLeftIcon className="size-4" aria-hidden="true" />
        </PagerButton>
        <PagerButton
          label="Previous page"
          onClick={() => onPageChange(Math.max(1, page - 1))}
          disabled={disabled || onFirstPage}
        >
          <ChevronLeftIcon className="size-4" aria-hidden="true" />
        </PagerButton>
        <PagerButton
          label="Next page"
          onClick={() => onPageChange(page + 1)}
          disabled={disabled || onLastPage}
        >
          <ChevronRightIcon className="size-4" aria-hidden="true" />
        </PagerButton>
        <PagerButton
          label="Last page"
          onClick={() => pageCount != null && onPageChange(pageCount)}
          disabled={disabled || onLastPage || pageCount == null}
        >
          <ChevronDoubleRightIcon className="size-4" aria-hidden="true" />
        </PagerButton>
      </div>
    </nav>
  );
}

function PagerButton({
  label,
  onClick,
  disabled,
  children,
}: {
  label:    string;
  onClick:  () => void;
  disabled: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
      className="inline-flex size-8 items-center justify-center rounded-md border border-line bg-panel/80 text-fg-3 transition-colors enabled:hover:bg-panel enabled:hover:text-fg-2 disabled:cursor-default disabled:opacity-40"
    >
      {children}
    </button>
  );
}
