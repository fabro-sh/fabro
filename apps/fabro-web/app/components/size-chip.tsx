import type { RunSize } from "@qltysh/fabro-api-client";

import { formatUsdMicros } from "../lib/format";
import { Tooltip } from "./ui";

const SIZE_TONE: Record<RunSize, { className: string; note: string | null }> = {
  XS: { className: "bg-overlay text-fg-muted",   note: null },
  S:  { className: "bg-overlay text-fg-muted",   note: null },
  M:  { className: "bg-overlay text-fg-muted",   note: null },
  L:  { className: "bg-amber/15 text-amber",     note: "risky" },
  XL: { className: "bg-coral/15 text-coral",     note: "unhealthy" },
};

export function SizeChip({
  size,
  totalUsdMicros,
}: {
  size:            RunSize;
  totalUsdMicros?: number | null;
}) {
  const tone = SIZE_TONE[size];
  const billed = totalUsdMicros != null ? ` · ${formatUsdMicros(totalUsdMicros)} billed` : "";
  const tooltip = tone.note != null
    ? `Size ${size} (${tone.note})${billed}`
    : `Size ${size}${billed}`;
  return (
    <Tooltip label={tooltip}>
      <span className={`rounded px-1.5 py-0.5 font-mono text-xs font-bold tabular-nums ${tone.className}`}>
        {size}
      </span>
    </Tooltip>
  );
}
