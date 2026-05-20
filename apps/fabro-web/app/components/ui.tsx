// Shared UI primitives. The install wizard set the visual baseline; this file
// exposes the primary button, secondary button, input, error message, and
// copy button so the auth and in-app surfaces can match.

import { useId, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Dialog, DialogPanel, DialogTitle } from "@headlessui/react";
import {
  ClipboardDocumentCheckIcon,
  ClipboardIcon,
} from "@heroicons/react/16/solid";

export const INPUT_CLASS =
  "block w-full rounded-lg bg-panel-alt px-3.5 py-2.5 text-base text-fg outline-1 -outline-offset-1 outline-white/10 placeholder:text-fg-muted focus:outline-2 focus:-outline-offset-1 focus:outline-teal-500 sm:text-sm";

export const PRIMARY_BUTTON_CLASS =
  "inline-flex items-center justify-center gap-2 rounded-lg bg-teal-500 px-4 py-2 text-sm font-medium text-on-primary transition-colors hover:bg-teal-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-teal-500";

export const SECONDARY_BUTTON_CLASS =
  "inline-flex items-center justify-center gap-2 rounded-lg bg-transparent px-3.5 py-2 text-sm font-medium text-fg-2 outline-1 -outline-offset-1 outline-white/10 hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60";

export const DANGER_BUTTON_CLASS =
  "inline-flex items-center justify-center gap-2 rounded-lg bg-coral px-4 py-2 text-sm font-medium text-on-primary transition-colors hover:bg-coral/90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-coral disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-coral";

export const COMPACT_SECONDARY_BUTTON_CLASS =
  "rounded-md border border-line bg-overlay px-2.5 py-1 text-xs text-fg-2 transition-colors hover:bg-overlay-strong hover:text-fg disabled:cursor-not-allowed disabled:opacity-50";

export function ErrorMessage({ message }: { message: string }) {
  return (
    <p
      role="alert"
      className="rounded-md bg-coral/10 px-3 py-2 text-sm/6 text-fg-2 outline-1 -outline-offset-1 outline-coral/40"
    >
      {message}
    </p>
  );
}

export function FormField({
  label,
  htmlFor,
  help,
  children,
}: {
  label: string;
  htmlFor: string;
  help?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div>
      <label htmlFor={htmlFor} className="mb-1.5 block text-sm font-medium text-fg-2">
        {label}
      </label>
      {children}
      {help ? <p className="mt-1.5 text-xs/5 text-fg-3 text-pretty">{help}</p> : null}
    </div>
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
      className={`inline-flex size-6 shrink-0 items-center justify-center rounded text-fg-muted outline-teal-500 hover:bg-overlay hover:text-fg-2 focus-visible:outline-2 focus-visible:outline-offset-1 ${className}`}
      aria-label={copied ? "Copied" : label}
      title={copied ? "Copied" : label}
    >
      {copied ? (
        <ClipboardDocumentCheckIcon className="size-4 text-mint" />
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
      <div className="fixed inset-0 bg-black/60 backdrop-blur-sm" aria-hidden="true" />
      <div className="fixed inset-0 flex items-start justify-center pt-[20vh] px-4">
        <DialogPanel className="w-full max-w-md rounded-lg border border-line-strong bg-panel shadow-2xl shadow-black/40">
          <div className="px-5 py-4">
            <DialogTitle className="text-sm font-semibold text-fg">{title}</DialogTitle>
            <div className="mt-2 text-sm text-fg-3">{description}</div>
            <div className="mt-5 flex justify-end gap-2">
              <button
                type="button"
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
                className={DANGER_BUTTON_CLASS}
              >
                {pending ? (pendingLabel ?? `${confirmLabel}…`) : confirmLabel}
              </button>
            </div>
          </div>
        </DialogPanel>
      </div>
    </Dialog>
  );
}

export function Tooltip({
  label,
  children,
}: {
  label: React.ReactNode;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLSpanElement>(null);
  const id = useId();
  const rect = open ? triggerRef.current?.getBoundingClientRect() : null;
  const portalTarget = typeof document === "undefined" ? null : document.body;

  return (
    <>
      <span
        ref={triggerRef}
        onMouseEnter={() => setOpen(true)}
        onMouseLeave={() => setOpen(false)}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        aria-describedby={open ? id : undefined}
        className="inline-flex"
      >
        {children}
      </span>
      {rect && portalTarget
        ? createPortal(
            <div
              role="tooltip"
              id={id}
              style={{
                top: rect.top - 6,
                left: rect.left + rect.width / 2,
              }}
              className="pointer-events-none fixed z-50 -translate-x-1/2 -translate-y-full whitespace-nowrap rounded-md bg-panel px-2 py-1 text-xs text-fg-2 shadow-lg outline-1 -outline-offset-1 outline-line-strong"
            >
              {label}
            </div>,
            portalTarget,
          )
        : null}
    </>
  );
}
