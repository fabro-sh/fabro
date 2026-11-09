import {
  CheckCircleIcon,
  ExclamationTriangleIcon,
  InformationCircleIcon,
  XCircleIcon,
} from "@heroicons/react/16/solid";
import type { ReactNode } from "react";
import { Toaster as SonnerToaster, toast as sonnerToast, useSonner } from "sonner";

import { useAppearance } from "./appearance-provider";

export type ToastTone = "info" | "error";

export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface ToastInput {
  message: string;
  tone?: ToastTone;
  /** Pass `Infinity` for a toast that stays until dismissed or acted on. */
  autoDismissMs?: number;
  action?: ToastAction;
}

interface ToastContextValue {
  push: (toast: ToastInput) => string;
  dismiss: (id: string) => void;
  clear: () => void;
}

let nextToastId = 0;

function push(toast: ToastInput): string {
  const id = `toast-${nextToastId++}`;
  const options = {
    id,
    ...(toast.action ? { action: toast.action } : {}),
    ...(toast.tone === "error"
      ? { duration: Infinity }
      : toast.autoDismissMs != null
        ? { duration: toast.autoDismissMs }
        : {}),
  };

  if (toast.tone === "error") {
    sonnerToast.error(toast.message, options);
  } else {
    sonnerToast(toast.message, options);
  }
  return id;
}

const toastApi: ToastContextValue = {
  push,
  dismiss: (id) => {
    sonnerToast.dismiss(id);
  },
  clear: () => {
    sonnerToast.dismiss();
  },
};

/**
 * No-op wrapper retained so existing test harnesses and the standalone terminal
 * route can keep their <ToastProvider> mount points. In a browser the real
 * <Toaster /> is mounted globally at the entry point; in non-DOM test
 * environments we render an aria-live fallback that subscribes to the Sonner
 * store so test assertions can read the toast text.
 */
export function ToastProvider({ children }: { children: ReactNode }) {
  if (typeof document !== "undefined") {
    return <>{children}</>;
  }
  return (
    <>
      {children}
      <NonDomToastOutput />
    </>
  );
}

export function useToast(): ToastContextValue {
  return toastApi;
}

/**
 * App-themed Sonner host. Renders toasts on the active palette's panel surface
 * with type-colored icons instead of Sonner's default green/red palette, and tells
 * Sonner the color mode so its own defaults agree. The `group`/`toaster`
 * + `group`/`toast` class pairing lets the `group-[.toaster]:` and
 * `group-[.toast]:` utilities out-specify Sonner's own `[data-sonner-toast]`
 * defaults without `!important`.
 */
export function FabroToaster() {
  const { appearance } = useAppearance();
  return (
    <SonnerToaster
      theme={appearance.mode}
      position="bottom-right"
      className="toaster group"
      // Notifications must not intercept clicks on modal dialogs (z-50 and above).
      style={{ zIndex: 40 }}
      closeButton
      icons={{
        success: <CheckCircleIcon className="size-4 shrink-0 fill-mint" />,
        error: <XCircleIcon className="size-4 shrink-0 fill-coral" />,
        info: <InformationCircleIcon className="size-4 shrink-0 fill-teal-500" />,
        warning: (
          <ExclamationTriangleIcon className="size-4 shrink-0 fill-amber" />
        ),
      }}
      toastOptions={{
        classNames: {
          toast:
            "group toast font-sans text-sm group-[.toaster]:bg-panel group-[.toaster]:text-fg-2 group-[.toaster]:rounded-lg group-[.toaster]:border-line-strong",
          title: "font-medium text-fg",
          description: "group-[.toast]:text-fg-3",
          closeButton:
            "group-[.toast]:bg-panel group-[.toast]:border-line-strong group-[.toast]:text-fg-3 group-[.toast]:hover:bg-overlay group-[.toast]:hover:text-fg",
          actionButton:
            "group-[.toast]:rounded-md group-[.toast]:bg-primary group-[.toast]:text-on-primary group-[.toast]:text-xs group-[.toast]:font-medium",
          cancelButton:
            "group-[.toast]:rounded-md group-[.toast]:bg-overlay group-[.toast]:text-fg-2 group-[.toast]:text-xs",
        },
      }}
    />
  );
}

function NonDomToastOutput() {
  const { toasts } = useSonner();
  if (toasts.length === 0) return null;
  return (
    <output aria-live="polite">
      {toasts.map((toast) => (
        <p key={toast.id}>
          {typeof toast.title === "function" ? toast.title() : toast.title}
        </p>
      ))}
    </output>
  );
}
