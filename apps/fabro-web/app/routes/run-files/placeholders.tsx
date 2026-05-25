import type { ReactElement } from "react";
import type {
  FileDiff as ApiFileDiff,
  RunFilesMetaDegradedReasonEnum,
} from "@qltysh/fabro-api-client";

const PLACEHOLDER_CLASSES =
  "flex items-center justify-between rounded-md border border-line bg-panel/60 px-4 py-3 text-sm text-fg-muted";

export function SensitivePlaceholder({ name }: { name: string }) {
  return (
    <div className={PLACEHOLDER_CLASSES}>
      <span className="font-mono text-fg-2">{name}</span>
      <span className="rounded bg-rose-950/40 px-2 py-0.5 text-xs text-rose-200">
        sensitive: contents omitted
      </span>
    </div>
  );
}

export function BinaryPlaceholder({ name }: { name: string }) {
  return (
    <div className={PLACEHOLDER_CLASSES}>
      <span className="font-mono text-fg-2">{name}</span>
      <span className="rounded bg-panel-alt/80 px-2 py-0.5 text-xs text-fg-3">
        binary: not shown inline
      </span>
    </div>
  );
}

export function TruncatedPlaceholder({
  name,
  reason,
}: {
  name: string;
  reason?: string;
}) {
  const label =
    reason === "budget_exhausted"
      ? "omitted — too many files changed"
      : "too large to render inline";
  return (
    <div className={PLACEHOLDER_CLASSES}>
      <span className="font-mono text-fg-2">{name}</span>
      <span className="rounded bg-panel-alt/80 px-2 py-0.5 text-xs text-fg-3">
        {label}
      </span>
    </div>
  );
}

export function SymlinkOrSubmodulePlaceholder({
  name,
  kind,
}: {
  name: string;
  kind: "symlink" | "submodule";
}) {
  return (
    <div className={PLACEHOLDER_CLASSES}>
      <span className="font-mono text-fg-2">{name}</span>
      <span className="rounded bg-panel-alt/80 px-2 py-0.5 text-xs text-fg-3">
        {kind}
      </span>
    </div>
  );
}

/// Render the highest-priority placeholder for a file, or `null` if the file
/// should render as a normal diff. Priority order is:
///   sensitive > binary > symlink/submodule > truncated
/// Security flags must never be hidden by a lesser placeholder.
export function pickPlaceholder(file: ApiFileDiff): ReactElement | null {
  const displayName = file.new_file.name || file.old_file.name;
  if (file.sensitive) {
    return <SensitivePlaceholder name={displayName} />;
  }
  if (file.binary) {
    return <BinaryPlaceholder name={displayName} />;
  }
  if (file.change_kind === "symlink") {
    return <SymlinkOrSubmodulePlaceholder name={displayName} kind="symlink" />;
  }
  if (file.change_kind === "submodule") {
    return (
      <SymlinkOrSubmodulePlaceholder name={displayName} kind="submodule" />
    );
  }
  if (file.truncated) {
    return (
      <TruncatedPlaceholder
        name={displayName}
        reason={file.truncation_reason}
      />
    );
  }
  return null;
}

export function DegradedBanner({
  reason,
}: {
  reason?: RunFilesMetaDegradedReasonEnum;
}) {
  const copy = bannerCopyForReason(reason);
  if (copy === null) return null;
  return (
    <div className="rounded-md border border-amber-500/30 bg-amber-950/20 px-4 py-3 text-sm text-amber-100">
      {copy}
    </div>
  );
}

export function bannerCopyForReason(
  reason: RunFilesMetaDegradedReasonEnum | undefined | string,
): string | null {
  switch (reason) {
    case "sandbox_gone":
      return null;
    case "provider_unsupported":
      return "Live diff isn't supported for this sandbox provider. Showing the patch captured at the last checkpoint.";
    case "sandbox_unreachable":
    default:
      return "Couldn't reach this run's sandbox. Showing the patch captured at the last checkpoint — refresh to try again.";
  }
}
