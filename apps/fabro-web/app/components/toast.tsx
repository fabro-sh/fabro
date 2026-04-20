import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { XMarkIcon } from "@heroicons/react/20/solid";

export type ToastTone = "info" | "error";

export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface ToastInput {
  message: string;
  tone?: ToastTone;
  action?: ToastAction;
  autoDismissMs?: number;
}

interface ToastRecord extends ToastInput {
  id: string;
  tone: ToastTone;
}

interface ToastContextValue {
  push: (toast: ToastInput) => string;
  dismiss: (id: string) => void;
  clear: () => void;
}

const ToastContext = createContext<ToastContextValue | null>(null);

function toastClassName(tone: ToastTone): string {
  return tone === "error"
    ? "border-coral/40 bg-rose-950/90 text-rose-50"
    : "border-line bg-panel/95 text-fg-2";
}

export function ToastRoot({
  toasts,
  onDismiss,
}: {
  toasts: ToastRecord[];
  onDismiss: (id: string) => void;
}) {
  if (toasts.length === 0) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      aria-atomic="false"
      className="pointer-events-none fixed right-4 bottom-6 z-50 flex w-[min(24rem,calc(100vw-2rem))] flex-col gap-2 sm:right-6"
    >
      {toasts.map((toast) => (
        <div
          key={toast.id}
          data-toast-id={toast.id}
          className={`pointer-events-auto rounded-lg border px-4 py-3 shadow-lg ${toastClassName(toast.tone)}`}
        >
          <div className="flex items-start gap-3">
            <p className="min-w-0 flex-1 text-sm leading-5">{toast.message}</p>
            {toast.tone === "error" && (
              <button
                type="button"
                onClick={() => onDismiss(toast.id)}
                className="inline-flex size-8 shrink-0 items-center justify-center rounded-md text-current/70 transition-colors hover:bg-white/10 hover:text-current focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
                aria-label="Dismiss notification"
              >
                <XMarkIcon className="size-4" aria-hidden="true" />
              </button>
            )}
          </div>
          {toast.action && (
            <div className="mt-3">
              <button
                type="button"
                onClick={() => {
                  toast.action?.onClick();
                  onDismiss(toast.id);
                }}
                className="inline-flex min-h-11 min-w-11 items-center justify-center rounded-md bg-white/10 px-3 text-sm font-medium text-current transition-colors hover:bg-white/15 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
              >
                {toast.action.label}
              </button>
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

export function ToastProvider({
  children,
  autoDismissMs = 3500,
}: {
  children: ReactNode;
  autoDismissMs?: number;
}) {
  const [toasts, setToasts] = useState<ToastRecord[]>([]);
  const nextIdRef = useRef(0);
  const timeoutIdsRef = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  const dismiss = useCallback((id: string) => {
    const timeoutId = timeoutIdsRef.current.get(id);
    if (timeoutId) {
      clearTimeout(timeoutId);
      timeoutIdsRef.current.delete(id);
    }
    setToasts((current) => current.filter((toast) => toast.id !== id));
  }, []);

  const clear = useCallback(() => {
    for (const timeoutId of timeoutIdsRef.current.values()) {
      clearTimeout(timeoutId);
    }
    timeoutIdsRef.current.clear();
    setToasts([]);
  }, []);

  const push = useCallback((toast: ToastInput) => {
    const id = `toast-${nextIdRef.current++}`;
    const record: ToastRecord = {
      ...toast,
      id,
      tone: toast.tone ?? "info",
    };

    setToasts((current) => [...current, record]);

    if (record.tone !== "error") {
      const timeoutId = setTimeout(() => dismiss(id), toast.autoDismissMs ?? autoDismissMs);
      timeoutIdsRef.current.set(id, timeoutId);
    }

    return id;
  }, [autoDismissMs, dismiss]);

  useEffect(() => clear, [clear]);

  const value = useMemo(() => ({ push, dismiss, clear }), [push, dismiss, clear]);

  return (
    <ToastContext.Provider value={value}>
      {children}
      <ToastRoot toasts={toasts} onDismiss={dismiss} />
    </ToastContext.Provider>
  );
}

export function useToast(): ToastContextValue {
  const value = useContext(ToastContext);
  if (!value) {
    throw new Error("useToast must be used within a ToastProvider");
  }
  return value;
}
