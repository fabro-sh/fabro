import type { ComponentType, ReactNode, SVGProps } from "react";
import { ExclamationTriangleIcon } from "@heroicons/react/20/solid";

import { SECONDARY_BUTTON_CLASS } from "./ui";

type IconComponent = ComponentType<SVGProps<SVGSVGElement>>;

// Shared chrome for non-critical "the content isn't there" states. Full-page
// crashes are handled by the root ErrorBoundary; these render inside the
// current content column.
function StatePanel({ children }: { children: ReactNode }) {
  return (
    <div className="mx-auto flex max-w-md flex-col items-center rounded-md border border-line bg-panel p-8 text-center">
      {children}
    </div>
  );
}

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
    <StatePanel>
      {Icon ? (
        <Icon className="mb-3 size-6 text-fg-muted" aria-hidden="true" />
      ) : null}
      <p className="text-sm font-medium text-fg">{title}</p>
      {description ? (
        <p className="mt-1 max-w-[48ch] text-sm/6 text-fg-3">{description}</p>
      ) : null}
      {action ? <div className="mt-4">{action}</div> : null}
    </StatePanel>
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
    <StatePanel>
      <ExclamationTriangleIcon
        className="mb-3 size-6 text-coral"
        aria-hidden="true"
      />
      <p className="text-sm font-medium text-fg">{title}</p>
      {description ? (
        <p className="mt-1 max-w-[48ch] text-sm/6 text-fg-3">{description}</p>
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
    </StatePanel>
  );
}

export function LoadingState({ label }: { label?: string }) {
  return (
    <StatePanel>
      <Spinner className="mb-3 size-5 text-teal-500" />
      <p className="text-sm text-fg-3">{label ?? "Loading…"}</p>
    </StatePanel>
  );
}

export function Spinner({ className = "" }: { className?: string }) {
  return (
    <svg
      className={`shrink-0 animate-spin ${className}`}
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
