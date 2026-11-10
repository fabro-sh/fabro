import {
  ChevronDoubleLeftIcon,
  ChevronDoubleRightIcon,
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
      className="flex flex-wrap items-center justify-between gap-x-6 gap-y-2 pt-2 text-[12px] font-bold text-muted"
    >
      <label htmlFor="runs-page-size" className="flex items-center gap-2">
        Rows per page
        <select
          id="runs-page-size"
          value={pageSize}
          onChange={(e) => onPageSizeChange(Number(e.target.value))}
          disabled={disabled}
          className="input h-8 w-auto"
        >
          {LIST_PAGE_SIZES.map((size) => (
            <option key={size} value={size}>{size}</option>
          ))}
        </select>
      </label>

      <span>
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
      className="btn btn-sm w-8 px-0"
    >
      {children}
    </button>
  );
}
