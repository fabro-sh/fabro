import type { ReactElement } from "react";
import { MultiFileDiff, PatchDiff } from "@pierre/diffs/react";
import { useTheme } from "../lib/theme";
import { apiJsonOrNull } from "../api";
import type {
  FileDiff as ApiFileDiff,
  PaginatedRunFileList,
} from "@qltysh/fabro-api-client";

export const handle = { wide: true };

export async function loader({ request, params }: any) {
  const data = await apiJsonOrNull<PaginatedRunFileList>(
    `/runs/${params.id}/files`,
    { request },
  );
  return data;
}

const PLACEHOLDER_CLASSES =
  "flex items-center justify-between rounded-md border border-line bg-panel/60 px-4 py-3 text-sm text-fg-muted";

function DegradedBanner({ reason }: { reason?: string }) {
  const copy = banner_copy_for_reason(reason);
  return (
    <div className="rounded-md border border-amber-500/30 bg-amber-950/20 px-4 py-3 text-sm text-amber-100">
      {copy}
    </div>
  );
}

function banner_copy_for_reason(reason: string | undefined): string {
  switch (reason) {
    case "sandbox_gone":
      return "Showing final patch only. This run's sandbox has been cleaned up, so individual file contents are no longer available.";
    case "provider_unsupported":
      return "Live diff isn't supported for this sandbox provider. Showing the patch captured at the last checkpoint.";
    case "sandbox_unreachable":
    default:
      return "Couldn't reach this run's sandbox. Showing the patch captured at the last checkpoint — refresh to try again.";
  }
}

function SensitivePlaceholder({ name }: { name: string }) {
  return (
    <div className={PLACEHOLDER_CLASSES}>
      <span className="font-mono text-fg-2">{name}</span>
      <span className="rounded bg-rose-950/40 px-2 py-0.5 text-xs text-rose-200">
        sensitive — contents omitted
      </span>
    </div>
  );
}

function BinaryPlaceholder({ name }: { name: string }) {
  return (
    <div className={PLACEHOLDER_CLASSES}>
      <span className="font-mono text-fg-2">{name}</span>
      <span className="rounded bg-panel-alt/80 px-2 py-0.5 text-xs text-fg-3">
        binary — not shown inline
      </span>
    </div>
  );
}

function TruncatedPlaceholder({
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

function SymlinkOrSubmodulePlaceholder({
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

function EmptyState({ message }: { message: string }) {
  return (
    <div className="rounded-md border border-dashed border-line bg-panel/40 px-6 py-10 text-center text-sm text-fg-muted">
      {message}
    </div>
  );
}

function pick_placeholder(file: ApiFileDiff): ReactElement | null {
  const display_name = file.new_file.name || file.old_file.name;
  // Priority: sensitive > binary > symlink/submodule > truncated. Security
  // flags must never be hidden by a lesser placeholder.
  if (file.sensitive) {
    return <SensitivePlaceholder name={display_name} />;
  }
  if (file.binary) {
    return <BinaryPlaceholder name={display_name} />;
  }
  if (file.change_kind === "symlink") {
    return <SymlinkOrSubmodulePlaceholder name={display_name} kind="symlink" />;
  }
  if (file.change_kind === "submodule") {
    return (
      <SymlinkOrSubmodulePlaceholder name={display_name} kind="submodule" />
    );
  }
  if (file.truncated) {
    return (
      <TruncatedPlaceholder
        name={display_name}
        reason={file.truncation_reason}
      />
    );
  }
  return null;
}

export default function RunFiles({ loaderData }: any) {
  const theme = useTheme();
  const data = loaderData as PaginatedRunFileList | null;

  if (!data) {
    return (
      <EmptyState message="The diff for this run is not available right now." />
    );
  }

  const { data: files, meta } = data;

  // Degraded: render the unified patch string directly. Deep-link file
  // targeting isn't available in this mode; Unit 12 will add a transient
  // toast pointing that out.
  if (meta.degraded && meta.patch) {
    return (
      <div className="flex flex-col gap-4">
        <DegradedBanner reason={meta.degraded_reason} />
        <PatchDiff
          patch={meta.patch}
          options={{
            diffStyle: "split",
            theme: theme.theme === "dark" ? "pierre-dark" : "pierre-light",
          }}
        />
      </div>
    );
  }

  if (files.length === 0) {
    return <EmptyState message="This run didn't change any files." />;
  }

  return (
    <div className="flex flex-col gap-4">
      {files.map((file, idx) => {
        const placeholder = pick_placeholder(file);
        if (placeholder) {
          return (
            <div
              key={`${file.new_file.name || file.old_file.name}-${idx}`}
              role="region"
              aria-label={`${file.change_kind ?? "changed"}: ${file.new_file.name || file.old_file.name}`}
            >
              {placeholder}
            </div>
          );
        }
        return (
          <div
            key={`${file.new_file.name}-${idx}`}
            role="region"
            aria-label={`${file.change_kind ?? "modified"}: ${file.new_file.name}`}
          >
            <MultiFileDiff
              oldFile={file.old_file}
              newFile={file.new_file}
              options={{
                diffStyle: "split",
                theme: theme.theme === "dark" ? "pierre-dark" : "pierre-light",
              }}
            />
          </div>
        );
      })}
    </div>
  );
}
