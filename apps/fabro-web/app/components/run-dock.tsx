import {
  useId,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
  type Ref,
} from "react";
import {
  ArrowUturnLeftIcon,
  ChevronUpIcon,
} from "@heroicons/react/20/solid";

import { classNames } from "../lib/class-names";
import { Spinner } from "./state";
import {
  INPUT_CLASS,
  PRIMARY_BUTTON_CLASS,
} from "./ui";

/**
 * Shared chrome for the two controls docked at the bottom of the run detail
 * route: the interview question panel and the steering composer.
 *
 * The shell is three zones. The header is always visible and doubles as the
 * collapsed bar. The body scrolls. The actions stay pinned, so the controls
 * needed to answer or send never scroll out of reach.
 *
 * Collapsed state is owned by the caller. Each dock has its own rule for when
 * a collapsed panel must reopen — a new question for the interview, a run
 * waiting for steering for the composer — and those rules are clearer next to
 * the state they depend on.
 */

/** Ceiling on the expanded dock, so a long body cannot take the page. */
const DOCK_MAX_HEIGHT = "max-h-[60vh]";

export const DOCK_HEADER_BUTTON =
  "inline-flex shrink-0 items-center gap-1.5 rounded-md bg-overlay px-2 py-1 text-xs font-medium text-fg-2 outline-1 -outline-offset-1 outline-line-strong transition-colors hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-overlay disabled:hover:text-fg-2";

export const DOCK_CHOICE_BUTTON =
  "inline-flex items-center justify-center gap-1.5 rounded-lg bg-overlay px-3.5 py-2 text-left text-sm font-medium text-fg-2 outline-1 -outline-offset-1 outline-line-strong transition-colors hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-current disabled:cursor-not-allowed disabled:opacity-60";

export const DOCK_CHOICE_BUTTON_SELECTED =
  "inline-flex items-center justify-center gap-1.5 rounded-lg bg-teal-500/15 px-3.5 py-2 text-left text-sm font-medium text-fg outline-1 -outline-offset-1 outline-teal-500/60 transition-colors hover:bg-teal-500/20 focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-current";

/**
 * How the dock signals its state.
 *
 * - `waiting` pulses amber: the run is blocked on the operator, as expected.
 * - `alert` pulses amber and colors the label too, for a run that went off
 *   its normal path and is stuck until someone acts.
 * - `idle` is a resting control with no pending demand.
 */
export type DockTone = "waiting" | "alert" | "idle";

export interface RunDockShellProps {
  /** Accessible name for the docked region. */
  label: string;
  className?: string;
  tone: DockTone;
  /** Short state phrase, e.g. "Awaiting input". */
  status: string;
  /** Stage name shown in mono beside the status. */
  stage?: string | null;
  /** One-line summary shown only while collapsed. */
  peek?: string | null;
  /** Run-level controls, rendered beside the collapse toggle. */
  headerActions?: ReactNode;
  /** Scrolling zone. Omit when there is nothing to scroll. */
  body?: ReactNode;
  /** Pinned zone. */
  actions: ReactNode;
  collapsed: boolean;
  onCollapsedChange: (collapsed: boolean) => void;
}

export function RunDockShell({
  label,
  className,
  tone,
  status,
  stage,
  peek,
  headerActions,
  body,
  actions,
  collapsed,
  onCollapsedChange,
}: RunDockShellProps) {
  const contentId = useId();

  return (
    <section
      aria-label={label}
      className={classNames(
        "flex flex-col",
        !collapsed && DOCK_MAX_HEIGHT,
        className,
      )}
    >
      {/* While collapsed, the whole bar expands on click. Clicks that land on
          a button (Interrupt, the chevron) keep their own behavior. The
          chevron button stays the keyboard/assistive-tech toggle. */}
      <div
        className={classNames(
          "flex shrink-0 items-center gap-2.5 px-5 py-2 sm:px-6",
          collapsed && "cursor-pointer",
        )}
        onClick={
          collapsed
            ? (event) => {
                if ((event.target as HTMLElement | null)?.closest?.("button")) {
                  return;
                }
                onCollapsedChange(false);
              }
            : undefined
        }
      >
        <StatusDot tone={tone} />
        {/* A live region: the dock changing to a state that needs the
            operator has to reach assistive tech, not only the eye. */}
        <span
          role="status"
          className={`shrink-0 text-sm font-medium ${
            tone === "alert" ? "text-amber" : "text-fg-2"
          }`}
        >
          {status}
        </span>
        {stage && (
          <>
            <span className="shrink-0 text-fg-muted" aria-hidden="true">
              ·
            </span>
            <span className="min-w-0 truncate font-mono text-xs text-fg-3">
              {stage}
            </span>
          </>
        )}
        {collapsed && peek ? (
          <span className="min-w-0 flex-1 truncate text-sm text-fg-3">
            · {peek}
          </span>
        ) : (
          <span className="flex-1" />
        )}
        {headerActions}
        <button
          type="button"
          onClick={() => onCollapsedChange(!collapsed)}
          aria-expanded={!collapsed}
          aria-controls={contentId}
          aria-label={collapsed ? `Expand ${label}` : `Collapse ${label}`}
          className="inline-flex size-6.5 shrink-0 items-center justify-center rounded-md text-fg-3 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current"
        >
          <ChevronUpIcon
            className={`size-4 transition-transform duration-200 ease-[cubic-bezier(0.16,1,0.3,1)] ${
              collapsed ? "rotate-180" : ""
            }`}
            aria-hidden="true"
          />
        </button>
      </div>

      {/* Hidden rather than unmounted, so a half-written message survives a
          collapse. The display utility is swapped rather than layered, so two
          display classes cannot collide in the cascade. */}
      <div
        id={contentId}
        className={collapsed ? "hidden" : "flex min-h-0 flex-1 flex-col"}
      >
        {body && (
          <div className="min-h-0 flex-1 space-y-3 overflow-y-auto border-t border-line px-5 pt-3.5 pb-1 sm:px-6">
            {body}
          </div>
        )}
        <div
          className={classNames(
            "flex max-h-[50%] shrink-0 flex-col gap-2.5 overflow-y-auto px-5 pt-2.5 pb-3.5 sm:px-6",
            !body && "border-t border-line",
          )}
        >
          {actions}
        </div>
      </div>
    </section>
  );
}

function StatusDot({ tone }: { tone: DockTone }) {
  if (tone === "idle") {
    return (
      <span
        className="size-2 shrink-0 rounded-full bg-fg-muted"
        aria-hidden="true"
      />
    );
  }
  return (
    <span
      className="relative flex size-2 shrink-0 items-center justify-center"
      aria-hidden="true"
    >
      <span className="absolute inline-flex size-full animate-ping rounded-full bg-amber/60" />
      <span className="relative inline-flex size-2 rounded-full bg-amber" />
    </span>
  );
}

export interface DockComposerProps {
  /**
   * Sends the trimmed text. Resolve `true` to clear the box; resolve `false`
   * to keep what the operator typed, so a failed send is not lost.
   */
  onSubmit: (text: string) => Promise<boolean>;
  placeholder: string;
  submitLabel: string;
  pendingLabel?: string;
  submitting: boolean;
  disabled?: boolean;
  ariaLabel: string;
  className?: string;
  maxLength?: number;
  textareaRef?: Ref<HTMLTextAreaElement>;
}

/**
 * The single composer used by both docks: the interview's freeform answer and
 * the steering message. Enter sends, Shift+Enter breaks the line, and the
 * hint for that only appears on focus — inside the row, so revealing it does
 * not shift the layout.
 */
export function DockComposer({
  onSubmit,
  placeholder,
  submitLabel,
  pendingLabel,
  submitting,
  disabled = false,
  ariaLabel,
  className,
  maxLength,
  textareaRef,
}: DockComposerProps) {
  const [value, setValue] = useState("");
  const fieldId = useId();
  const instructionId = `${fieldId}-instruction`;
  const trimmed = value.trim();
  const composerDisabled = disabled || submitting;
  const canSend = trimmed.length > 0 && !composerDisabled;

  async function send() {
    if (!canSend) return;
    const cleared = await onSubmit(trimmed);
    if (cleared) setValue("");
  }

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void send();
  }

  function handleKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      !event.nativeEvent.isComposing
    ) {
      event.preventDefault();
      void send();
    }
  }

  return (
    <form
      onSubmit={handleSubmit}
      className={classNames("group flex items-end gap-2", className)}
    >
      <label className="sr-only" htmlFor={fieldId}>
        {ariaLabel}
      </label>
      <p id={instructionId} className="sr-only">
        Press Enter to send. Press Shift+Enter for a new line.
      </p>
      <textarea
        id={fieldId}
        ref={textareaRef}
        aria-label={ariaLabel}
        aria-describedby={instructionId}
        rows={1}
        value={value}
        maxLength={maxLength}
        onChange={(event) => setValue(event.target.value)}
        onKeyDown={handleKeyDown}
        placeholder={placeholder}
        disabled={composerDisabled}
        className={`${INPUT_CLASS} min-w-0 flex-1 resize-none disabled:opacity-60`}
      />
      <p
        aria-hidden="true"
        className="pointer-events-none hidden shrink-0 items-center gap-1 pb-2.5 text-xs whitespace-nowrap text-fg-muted opacity-0 transition-opacity group-focus-within:opacity-100 md:flex"
      >
        <kbd className="rounded bg-overlay px-1 font-mono text-[0.6875rem]">
          Enter
        </kbd>
        send
        <kbd className="rounded bg-overlay px-1 font-mono text-[0.6875rem]">
          Shift
        </kbd>
        +
        <kbd className="rounded bg-overlay px-1 font-mono text-[0.6875rem]">
          Enter
        </kbd>
        newline
      </p>
      <button
        type="submit"
        disabled={!canSend}
        className={PRIMARY_BUTTON_CLASS}
      >
        {submitting ? (
          <Spinner className="size-4" />
        ) : (
          <ArrowUturnLeftIcon
            className="size-3.5 -scale-x-100"
            aria-hidden="true"
          />
        )}
        {submitting && pendingLabel ? pendingLabel : submitLabel}
      </button>
    </form>
  );
}
