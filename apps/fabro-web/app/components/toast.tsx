import type { ReactNode } from "react";
import { toast as sonnerToast, useSonner } from "sonner";

export type ToastTone = "info" | "error";

export interface ToastInput {
  message: string;
  tone?: ToastTone;
  autoDismissMs?: number;
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
 * <Toaster /> is mounted globally in AppShell; in non-DOM test environments we
 * render an aria-live fallback that subscribes to the Sonner store so test
 * assertions can read the toast text.
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
