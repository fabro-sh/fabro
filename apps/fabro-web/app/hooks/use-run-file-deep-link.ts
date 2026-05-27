import { useEffect, useRef } from "react";

import type { PaginatedRunFileList } from "@qltysh/fabro-api-client";
import type { ToastInput } from "../components/toast";

/**
 * Synchronizes the run-files URL hash with rendered file-row DOM focus and the
 * toast system. Missing-file toasts are deduped by key, and no persistent
 * browser resource is created.
 */
export function useRunFileDeepLinkFocus({
  data,
  hashFile,
  rowId,
  resolveToast,
  push,
}: {
  data: PaginatedRunFileList | null;
  hashFile: string | null;
  rowId: (path: string) => string;
  resolveToast: (
    hashFile: string | null,
    data: PaginatedRunFileList | null,
  ) => { key: string; message: string } | null;
  push: (toast: ToastInput) => string;
}) {
  const lastToastRef = useRef<string | null>(null);

  useEffect(() => {
    const toast = resolveToast(hashFile, data);
    if (toast) {
      if (lastToastRef.current !== toast.key) {
        push({ message: toast.message, autoDismissMs: 5000 });
        lastToastRef.current = toast.key;
      }
      return;
    }

    lastToastRef.current = null;
    if (!hashFile || !data) return;
    const el = document.getElementById(rowId(hashFile));
    if (el) {
      el.scrollIntoView({ block: "start", behavior: "smooth" });
      el.focus({ preventScroll: true });
    }
  }, [data, hashFile, push, resolveToast, rowId]);
}
