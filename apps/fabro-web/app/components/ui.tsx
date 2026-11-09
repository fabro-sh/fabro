// Shared UI primitives, drawn from Factory's component library in
// styles/factory-components.css. Dialog, tooltip and hover-card mechanics stay on
// HeadlessUI and Radix. Which parts are one-to-one: styles/FACTORY_COMPONENTS.md.

import { Dialog, DialogPanel, DialogTitle } from "@headlessui/react";
import {
  ClipboardDocumentCheckIcon,
  ClipboardIcon,
  XMarkIcon,
} from "@heroicons/react/16/solid";
import * as HoverCardPrimitive from "@radix-ui/react-hover-card";
import * as TooltipPrimitive from "@radix-ui/react-tooltip";
import {
  createContext,
  useContext,
  useState,
  type ComponentProps,
  type ReactNode,
} from "react";

export const INPUT_CLASS = "input";

export const PRIMARY_BUTTON_CLASS = "btn btn-primary";

export const SECONDARY_BUTTON_CLASS = "btn";

export const DANGER_BUTTON_CLASS = "btn btn-danger";

/** Icon and quiet actions, such as a dialog's close button. */
export const GHOST_BUTTON_CLASS = "btn btn-ghost";

/** The palette's highlight. Only where Factory itself uses a signal call to action. */
export const SIGNAL_BUTTON_CLASS = "btn btn-signal";

export const COMPACT_SECONDARY_BUTTON_CLASS = "btn btn-sm";

/** A filter or view control beside a search field. The same 36px height as Factory's filter row. */
export const TOOLBAR_BUTTON_CLASS = "btn btn-sm h-9";

export const TOOLTIP_SURFACE_CLASS = "tooltip-surface";

export const POPOVER_SURFACE_CLASS = "popover-surface";

export function ErrorMessage({ message }: { message: string }) {
  return (
    <p
      role="alert"
      className="rounded-md bg-bad-bg px-3 py-2 text-[13px] font-semibold text-bad"
    >
      {message}
    </p>
  );
}

export function CopyButton({
  value,
  label,
  className = "",
}: {
  value: string;
  label: string;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(value);
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        } catch {
          // Clipboard may be blocked; leave state unchanged.
        }
      }}
      className={`inline-flex size-6 shrink-0 items-center justify-center rounded-xs text-muted hover:bg-surface-3 hover:text-fg-2 ${className}`}
      aria-label={copied ? "Copied" : label}
      title={copied ? "Copied" : label}
    >
      {copied ? (
        <ClipboardDocumentCheckIcon className="size-4 text-ok" />
      ) : (
        <ClipboardIcon className="size-4" />
      )}
    </button>
  );
}

export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel,
  pendingLabel,
  cancelLabel = "Cancel",
  tone = "danger",
  pending = false,
  onConfirm,
  onCancel,
}: {
  open: boolean;
  title: string;
  description: ReactNode;
  confirmLabel: string;
  pendingLabel?: string;
  cancelLabel?: string;
  tone?: "primary" | "danger";
  pending?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Dialog
      open={open}
      onClose={() => {
        if (!pending) onCancel();
      }}
      className="relative z-50"
    >
      <div className="modal-backdrop fixed inset-0" aria-hidden="true" />
      <div className="fixed inset-0 flex items-center justify-center p-4">
        <DialogPanel className="modal modal-confirm flex flex-col">
          <div className="flex items-center justify-between gap-3 border-b border-line px-5 py-3">
            <DialogTitle className="text-base font-extrabold">{title}</DialogTitle>
            <button
              type="button"
              onClick={onCancel}
              disabled={pending}
              aria-label="Close"
              className={`${GHOST_BUTTON_CLASS} btn-sm`}
            >
              <XMarkIcon className="size-4" aria-hidden="true" />
            </button>
          </div>
          <div className="scroll-pane min-h-0 flex-1 px-5 py-4 text-[13px] text-fg-2">
            {description}
          </div>
          <div className="flex flex-wrap justify-end gap-2 border-t border-line px-5 py-3">
            {/* Opening a destructive question lands on the safe answer, not on the close icon. */}
            <button
              type="button"
              data-autofocus
              onClick={onCancel}
              disabled={pending}
              className={SECONDARY_BUTTON_CLASS}
            >
              {cancelLabel}
            </button>
            <button
              type="button"
              onClick={onConfirm}
              disabled={pending}
              className={tone === "danger" ? DANGER_BUTTON_CLASS : PRIMARY_BUTTON_CLASS}
            >
              {pending ? (pendingLabel ?? `${confirmLabel}…`) : confirmLabel}
            </button>
          </div>
        </DialogPanel>
      </div>
    </Dialog>
  );
}

const TOOLTIP_DELAY_DURATION = 200;
const TOOLTIP_SKIP_DELAY_DURATION = 300;

const HasTooltipProviderContext = createContext(false);

type TooltipProviderProps = ComponentProps<typeof TooltipPrimitive.Provider>;

function canUseOverlayDom() {
  return typeof window !== "undefined" && typeof document !== "undefined";
}

export function TooltipProvider({
  delayDuration = TOOLTIP_DELAY_DURATION,
  skipDelayDuration = TOOLTIP_SKIP_DELAY_DURATION,
  children,
  ...props
}: TooltipProviderProps) {
  if (!canUseOverlayDom()) {
    return <>{children}</>;
  }
  return (
    <HasTooltipProviderContext.Provider value={true}>
      <TooltipPrimitive.Provider
        delayDuration={delayDuration}
        skipDelayDuration={skipDelayDuration}
        {...props}
      >
        {children}
      </TooltipPrimitive.Provider>
    </HasTooltipProviderContext.Provider>
  );
}

export function Tooltip({
  label,
  children,
  side = "top",
  triggerClassName = "inline-flex",
  disabled = false,
}: {
  label: ReactNode;
  children: ReactNode;
  side?: "top" | "right" | "bottom" | "left";
  /** The wrapper's display. A full-width row needs something other than the inline default. */
  triggerClassName?: string;
  /** Keep the trigger mounted while suppressing its tooltip. */
  disabled?: boolean;
}) {
  // Tests and isolated mounts may render <Tooltip> without an ancestor
  // <TooltipProvider>; Radix throws in that case, so supply a local provider.
  const hasProvider = useContext(HasTooltipProviderContext);
  const [open, setOpen] = useState(false);
  if (disabled && open) setOpen(false);

  if (!canUseOverlayDom()) {
    return <span className={triggerClassName}>{children}</span>;
  }

  const root = (
    <TooltipPrimitive.Root open={!disabled && open} onOpenChange={(next) => setOpen(!disabled && next)}>
      <TooltipPrimitive.Trigger asChild>
        <span className={triggerClassName}>{children}</span>
      </TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content
          side={side}
          align="center"
          sideOffset={6}
          collisionPadding={12}
          className={`pointer-events-none z-50 max-w-[min(32rem,calc(100vw-1.5rem))] ${TOOLTIP_SURFACE_CLASS}`}
        >
          {label}
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );

  return hasProvider ? root : <TooltipProvider>{root}</TooltipProvider>;
}

/**
 * Hover-triggered rich popover. Unlike `Tooltip`, it anchors below the trigger,
 * stays within the viewport, and allows multi-line wrapping content. The
 * `content` node is mounted only while open, so consumers may fetch lazily.
 */
export function HoverCard({
  content,
  children,
  className = "inline-flex",
  openDelay = 0,
}: {
  content: ReactNode;
  children: ReactNode;
  className?: string;
  openDelay?: number;
}) {
  if (!canUseOverlayDom()) {
    return <span className={className}>{children}</span>;
  }
  return (
    <HoverCardPrimitive.Root openDelay={openDelay} closeDelay={0}>
      <HoverCardPrimitive.Trigger asChild>
        <span className={className}>{children}</span>
      </HoverCardPrimitive.Trigger>
      <HoverCardPrimitive.Portal>
        <HoverCardPrimitive.Content
          side="bottom"
          align="start"
          sideOffset={6}
          collisionPadding={12}
          className={`pointer-events-none z-50 ${POPOVER_SURFACE_CLASS}`}
        >
          {content}
        </HoverCardPrimitive.Content>
      </HoverCardPrimitive.Portal>
    </HoverCardPrimitive.Root>
  );
}

export function PopoverHeader({ children }: { children: ReactNode }) {
  return (
    <div className="mb-1.5 border-b border-line pb-1 text-[13px] font-extrabold text-fg">
      {children}
    </div>
  );
}

export function PopoverRows({ children }: { children: ReactNode }) {
  return <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">{children}</dl>;
}

export function PopoverRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="font-semibold text-muted">{label}</dt>
      <dd className="min-w-0 text-fg">{children}</dd>
    </>
  );
}
