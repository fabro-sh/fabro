import type { ComponentType, ReactNode, SVGProps } from "react";
import { ExclamationTriangleIcon } from "@heroicons/react/20/solid";

import { SECONDARY_BUTTON_CLASS } from "./ui";

type IconComponent = ComponentType<SVGProps<SVGSVGElement>>;

// Non-critical "the content isn't there" states, with Factory's empty and notice
// surfaces. Full-page crashes are handled by the root ErrorBoundary; these render
// inside the current content column.
export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
}: {
  icon?: IconComponent;
  title: string;
  description?: string;
  action?: ReactNode;
}) {
  return (
    <div className="mx-auto flex max-w-md flex-col items-center rounded-md border border-dashed border-line-strong px-4 py-6 text-center">
      {Icon ? (
        <Icon className="mb-3 size-6 text-muted" aria-hidden="true" />
      ) : null}
      <p className="text-sm font-extrabold text-fg">{title}</p>
      {description ? (
        <p className="mt-1 max-w-[48ch] text-[13px] text-muted">{description}</p>
      ) : null}
      {action ? <div className="mt-4">{action}</div> : null}
    </div>
  );
}

export function ErrorState({
  title = "Something went wrong",
  description,
  onRetry,
}: {
  title?: string;
  description?: string;
  onRetry?: () => void;
}) {
  return (
    <div
      role="alert"
      className="mx-auto flex max-w-md flex-col items-center rounded-md bg-bad-bg px-4 py-6 text-center text-bad"
    >
      <ExclamationTriangleIcon className="mb-3 size-6" aria-hidden="true" />
      <p className="text-sm font-extrabold">{title}</p>
      {description ? (
        <p className="mt-1 max-w-[48ch] text-[13px] font-semibold">{description}</p>
      ) : null}
      {onRetry ? (
        <button
          type="button"
          onClick={onRetry}
          className={`${SECONDARY_BUTTON_CLASS} mt-4`}
        >
          Try again
        </button>
      ) : null}
    </div>
  );
}

export function LoadingState({ label }: { label?: string }) {
  return (
    <div
      role="status"
      aria-busy="true"
      className="mx-auto flex max-w-md flex-col items-center p-6 text-center"
    >
      <Spinner className="mb-3 size-5 text-signal" />
      <p className="text-sm text-muted">{label ?? "Loading…"}</p>
    </div>
  );
}

export function Spinner({ className = "" }: { className?: string }) {
  return (
    <svg
      className={`shrink-0 animate-spin motion-reduce:animate-none ${className}`}
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
    >
      <circle cx="8" cy="8" r="6" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2" />
      <path
        d="M14 8a6 6 0 0 0-6-6"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
      />
    </svg>
  );
}
