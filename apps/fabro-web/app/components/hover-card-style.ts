import type { CSSProperties } from "react";

export function hoverCardStyle(rect: DOMRect): CSSProperties {
  const margin = 12;
  const top = rect.bottom + 6;
  if (rect.left > window.innerWidth / 2) {
    return { top, right: Math.max(margin, window.innerWidth - rect.right) };
  }
  return { top, left: Math.max(margin, rect.left) };
}
