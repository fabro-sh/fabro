import { useEffect, useMemo, useState } from "react";
import { useParams } from "react-router";
import { ArrowDownTrayIcon, PaperClipIcon } from "@heroicons/react/24/outline";
import type { RunArtifactEntry } from "@qltysh/fabro-api-client";

import { EmptyState, ErrorState, LoadingState } from "../components/state";
import { StageSidebar } from "../components/stage-sidebar";
import { formatBytes } from "../lib/format";
import { stageArtifactDownloadUrl } from "../lib/api-client";
import { useRunArtifacts, useRunStages } from "../lib/queries";
import { formatStageLabel, mapRunStagesToSidebarStages } from "../lib/stage-sidebar";

export const handle = { wide: true };

export default function RunArtifacts() {
  const { id } = useParams();
  const stagesQuery = useRunStages(id);
  const artifactsQuery = useRunArtifacts(id);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );

  return (
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id!} activeLink="artifacts" />
      <div className="min-w-0 flex-1">
        <RunArtifactsBody runId={id!} artifactsQuery={artifactsQuery} stages={stages} />
      </div>
    </div>
  );
}

function RunArtifactsBody({
  runId,
  artifactsQuery,
  stages,
}: {
  runId: string;
  artifactsQuery: ReturnType<typeof useRunArtifacts>;
  stages: ReturnType<typeof mapRunStagesToSidebarStages>;
}) {
  if (artifactsQuery.error) {
    return (
      <ErrorState
        title="Couldn't load artifacts"
        description={errorMessage(artifactsQuery.error)}
        onRetry={() => void artifactsQuery.mutate()}
      />
    );
  }
  if (artifactsQuery.data === undefined) {
    return <LoadingState label="Loading artifacts…" />;
  }
  const entries = artifactsQuery.data?.data ?? [];
  if (entries.length === 0) {
    return (
      <EmptyState
        icon={PaperClipIcon}
        title="No artifacts captured"
        description="No stage in this run produced any artifacts."
      />
    );
  }
  return <ArtifactList runId={runId} entries={entries} stages={stages} />;
}

interface StageGroup {
  key: string;
  stageId: string;
  retry: number;
  label: string;
  entries: RunArtifactEntry[];
  totalBytes: number;
}

function groupArtifacts(
  entries: readonly RunArtifactEntry[],
  stages: ReturnType<typeof mapRunStagesToSidebarStages>,
): StageGroup[] {
  const stageLabels = new Map<string, string>();
  for (const stage of stages) {
    stageLabels.set(stage.id, formatStageLabel(stage));
  }

  const groups = new Map<string, StageGroup>();
  for (const entry of entries) {
    const key = `${entry.stage_id}#${entry.retry}`;
    const existing = groups.get(key);
    if (existing) {
      existing.entries.push(entry);
      existing.totalBytes += entry.size;
    } else {
      groups.set(key, {
        key,
        stageId: entry.stage_id,
        retry: entry.retry,
        label: stageLabels.get(entry.stage_id) ?? entry.node_slug,
        entries: [entry],
        totalBytes: entry.size,
      });
    }
  }

  for (const group of groups.values()) {
    group.entries.sort((a, b) => a.relative_path.localeCompare(b.relative_path));
  }
  const sortedGroups = Array.from(groups.values());
  sortedGroups.sort((a, b) => {
    const labelCmp = a.label.localeCompare(b.label);
    return labelCmp !== 0 ? labelCmp : a.retry - b.retry;
  });
  return sortedGroups;
}

function ArtifactList({
  runId,
  entries,
  stages,
}: {
  runId: string;
  entries: readonly RunArtifactEntry[];
  stages: ReturnType<typeof mapRunStagesToSidebarStages>;
}) {
  const groups = useMemo(() => groupArtifacts(entries, stages), [entries, stages]);
  const totalBytes = useMemo(
    () => entries.reduce((sum, entry) => sum + entry.size, 0),
    [entries],
  );

  return (
    <div className="space-y-4">
      <div className="flex items-baseline justify-between">
        <h2 className="text-sm font-medium text-fg">
          {entries.length} {entries.length === 1 ? "artifact" : "artifacts"}
        </h2>
        <span className="text-xs tabular-nums text-fg-muted">
          {formatBytes(totalBytes)} total
        </span>
      </div>

      {groups.map((group) => (
        <StageGroupCard key={group.key} runId={runId} group={group} />
      ))}
    </div>
  );
}

function StageGroupCard({ runId, group }: { runId: string; group: StageGroup }) {
  return (
    <section className="overflow-hidden rounded-md border border-line bg-panel-alt">
      <header className="flex items-baseline justify-between border-b border-line px-4 py-2.5">
        <div className="flex items-baseline gap-2">
          <h3 className="text-sm font-medium text-fg">{group.label}</h3>
          {group.retry > 0 && (
            <span className="rounded bg-overlay px-1.5 py-0.5 text-[11px] font-medium text-fg-3">
              retry {group.retry}
            </span>
          )}
        </div>
        <span className="text-xs tabular-nums text-fg-muted">
          {group.entries.length} {group.entries.length === 1 ? "file" : "files"}
          {" · "}
          {formatBytes(group.totalBytes)}
        </span>
      </header>
      <ul className="divide-y divide-line">
        {group.entries.map((entry) => (
          <ArtifactRow
            key={`${group.key}#${entry.relative_path}`}
            runId={runId}
            entry={entry}
          />
        ))}
      </ul>
    </section>
  );
}

function ArtifactRow({ runId, entry }: { runId: string; entry: RunArtifactEntry }) {
  const [href, setHref] = useState<string>("#");

  useEffect(() => {
    let active = true;
    void stageArtifactDownloadUrl(
      runId,
      entry.stage_id,
      entry.relative_path,
      entry.retry,
    ).then((url) => {
      if (active) setHref(url);
    });
    return () => {
      active = false;
    };
  }, [entry.relative_path, entry.retry, entry.stage_id, runId]);

  return (
    <li className="flex items-center gap-4 px-4 py-2">
      <span
        className="flex-1 truncate font-mono text-xs text-fg-2"
        title={entry.relative_path}
      >
        {entry.relative_path}
      </span>
      <span className="shrink-0 tabular-nums text-xs text-fg-muted">
        {formatBytes(entry.size)}
      </span>
      <a
        href={href}
        download={basename(entry.relative_path)}
        className="inline-flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-xs text-fg-3 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500"
      >
        <ArrowDownTrayIcon className="size-3.5" aria-hidden="true" />
        Download
      </a>
    </li>
  );
}

function basename(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx >= 0 ? path.slice(idx + 1) : path;
}

function errorMessage(error: unknown): string | undefined {
  return error instanceof Error ? error.message : undefined;
}
