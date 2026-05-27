import type { RefObject } from "react";

import { useDocumentEvent } from "../../hooks/effects";

export function isEditableElement(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName.toLowerCase();
  if (tag === "input" || tag === "textarea" || tag === "select") return true;
  return (el as HTMLElement).isContentEditable === true;
}

/**
 * Wire keyboard navigation across file rows. `j` / `k` move focus to the
 * next / previous row. Key presses while a text-editable element is focused
 * are left alone so typing into filters / comment boxes isn't hijacked.
 *
 * Enter/Space are deliberately not bound — @pierre/diffs 1.1.x has no
 * imperative expand/collapse API, and firing a click on the outer wrapper
 * has no effect. Per-hunk expansion remains mouse-driven via pierre's own
 * controls. Files targeted by a deep-link get `expandUnchanged: true` via
 * per-file options instead.
 */
export function useFileKeyboardNav(
  containerRef: RefObject<HTMLDivElement | null>,
  fileCount: number,
) {
  useDocumentEvent(
    "keydown",
    (event) => {
      if (event.key !== "j" && event.key !== "k") return;
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (isEditableElement(document.activeElement)) return;
      const container = containerRef.current;
      if (!container) return;
      const rows = Array.from(
        container.querySelectorAll<HTMLElement>('[data-run-file-row="true"]'),
      );
      if (rows.length === 0) return;

      const active = document.activeElement as HTMLElement | null;
      const currentIdx = rows.findIndex((row) => row.contains(active));

      let nextIdx: number;
      if (currentIdx < 0) {
        nextIdx = 0;
      } else {
        nextIdx = event.key === "j" ? currentIdx + 1 : currentIdx - 1;
      }
      if (nextIdx < 0 || nextIdx >= rows.length) return;
      event.preventDefault();
      const target = rows[nextIdx];
      target.focus({ preventScroll: false });
      target.scrollIntoView({ block: "nearest", behavior: "smooth" });
    },
    undefined,
    fileCount > 0,
  );
}
