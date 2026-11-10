import { useMemo } from "react";
import { useParams } from "react-router";
import { Disclosure, DisclosureButton, DisclosurePanel } from "@headlessui/react";
import { ArrowDownTrayIcon, ChevronRightIcon, PaperClipIcon } from "@heroicons/react/24/outline";
import type { RunArtifactEntry } from "@qltysh/fabro-api-client";

import { EmptyState, ErrorState, LoadingState } from "../components/state";
import { StageSidebar } from "../components/stage-sidebar";
import {
  runArtifactsDownloadUrl,
  stageArtifactDownloadUrl,
} from "../lib/api-client";
import { formatBytes } from "../lib/format";
import { plural } from "../lib/plural";
import { useRunArtifacts, useRunStages } from "../lib/queries";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";
import type { ArtifactFile, ArtifactVersion } from "./run-artifacts/group";
import { groupArtifactsByFile } from "./run-artifacts/group";

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
        <RunArtifactsBody
          runId={id!}
          artifactsQuery={artifactsQuery}
          stagesQuery={stagesQuery}
          stages={stages}
        />
      </div>
    </div>
  );
}

function RunArtifactsBody({
  runId,
  artifactsQuery,
  stagesQuery,
  stages,
}: {
  runId: string;
  artifactsQuery: ReturnType<typeof useRunArtifacts>;
  stagesQuery: ReturnType<typeof useRunStages>;
  stages: ReturnType<typeof mapRunStagesToSidebarStages>;
}) {
  const error = artifactsQuery.error ?? stagesQuery.error;
  if (error) {
    return (
      <ErrorState
        title="Couldn't load artifacts"
        description={errorMessage(error)}
        onRetry={() => {
          if (artifactsQuery.error) void artifactsQuery.mutate();
          if (stagesQuery.error) void stagesQuery.mutate();
        }}
      />
    );
  }
  if (artifactsQuery.data === undefined || stagesQuery.data === undefined) {
    return <LoadingState label="Loading artifacts…" />;
  }
  return (
    <ArtifactFiles
      runId={runId}
      entries={artifactsQuery.data?.data ?? []}
      stages={stages}
    />
  );
}

function ArtifactFiles({
  runId,
  entries,
  stages,
}: {
  runId: string;
  entries: readonly RunArtifactEntry[];
  stages: ReturnType<typeof mapRunStagesToSidebarStages>;
}) {
  const files = useMemo(() => groupArtifactsByFile(entries, stages), [entries, stages]);

  if (files.length === 0) {
    return (
      <EmptyState
        icon={PaperClipIcon}
        title="No artifacts captured"
        description="No files were captured. Check the run’s Events for capture notices."
      />
    );
  }
  return <ArtifactList runId={runId} files={files} />;
}

function ArtifactList({ runId, files }: { runId: string; files: readonly ArtifactFile[] }) {
  const { captures, latestBytes, storedBytes } = useMemo(() => {
    let captures = 0;
    let latestBytes = 0;
    let storedBytes = 0;
    for (const file of files) {
      captures += file.versions.length;
      latestBytes += file.versions[0].size;
      for (const version of file.versions) storedBytes += version.size;
    }
    return { captures, latestBytes, storedBytes };
  }, [files]);

  // Only mention versions once some file actually has more than one.
  const versioned = captures > files.length;

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="text-sm font-medium text-fg">
          {files.length} {plural(files.length, "file", "files")}
          {versioned && (
            <span className="font-normal text-fg-muted">
              {" "}
              · {captures} {plural(captures, "version", "versions")}
            </span>
          )}
        </h2>
        <div className="ml-auto flex max-w-full flex-wrap items-center justify-end gap-3">
          <span className="text-xs text-fg-muted tabular-nums">
            {versioned
              ? `${formatBytes(latestBytes)} latest · ${formatBytes(storedBytes)} stored`
              : `${formatBytes(latestBytes)} total`}
          </span>
          <a
            href={runArtifactsDownloadUrl(runId)}
            aria-label={`Download all ${files.length} ${plural(files.length, "artifact", "artifacts")} as a ZIP file`}
            className="inline-flex min-h-11 shrink-0 items-center gap-1.5 rounded-md bg-overlay px-2.5 py-1 text-xs font-medium text-fg-2 outline-1 -outline-offset-1 outline-line-strong hover:bg-overlay-strong hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current sm:min-h-8"
          >
            <ArrowDownTrayIcon className="size-3.5 shrink-0" aria-hidden="true" />
            Download all
          </a>
        </div>
      </div>

      <section className="overflow-hidden rounded-md border border-line bg-panel-alt">
        {files.map((file) => (
          <ArtifactFileRow key={file.path} runId={runId} file={file} />
        ))}
      </section>
    </div>
  );
}

function ArtifactFileRow({ runId, file }: { runId: string; file: ArtifactFile }) {
  const hasEarlier = file.versions.length > 1;
  const latest = file.versions[0];

  return (
    <Disclosure as="div" className="border-t border-line first:border-t-0">
      {({ open }) => (
        <>
          <div className="flex items-center gap-2 px-3 py-2.5 sm:gap-4 sm:px-4">
            {hasEarlier ? (
              <DisclosureButton className="group shrink-0 rounded-md p-1 text-fg-3 transition-colors hover:bg-overlay hover:text-fg-2 focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-current">
                <span className="sr-only">
                  {open ? "Hide" : "Show"} earlier versions of {file.name}
                </span>
                <ChevronRightIcon
                  className="size-3.5 transition-transform group-data-open:rotate-90"
                  aria-hidden="true"
                />
              </DisclosureButton>
            ) : (
              <span className="size-5 shrink-0" aria-hidden="true" />
            )}

            <span className="min-w-0 flex-1" title={file.path}>
              <span className="block truncate font-mono text-xs">
                <span className="text-fg-muted">{file.dir}</span>
                <span className="text-fg-2">{file.name}</span>
              </span>
              <span className="mt-0.5 block truncate text-[11px] text-fg-3 md:hidden">
                <VersionLabel version={latest} />
              </span>
            </span>

            {hasEarlier && (
              <span className="hidden shrink-0 rounded-full bg-overlay-strong px-2 py-0.5 text-[11px] text-fg-3 lg:inline">
                {file.versions.length}{" "}
                {plural(file.versions.length, "version", "versions")}
              </span>
            )}

            <span className="hidden max-w-48 shrink-0 truncate text-xs text-fg-3 md:inline">
              <VersionLabel version={latest} />
            </span>
            <span className="shrink-0 text-xs text-fg-muted tabular-nums">
              {formatBytes(latest.size)}
            </span>
            <DownloadLink runId={runId} file={file} version={latest} />
          </div>

          {hasEarlier && (
            <DisclosurePanel
              as="ul"
              className="border-t border-line bg-overlay py-1"
            >
              <EarlierVersions runId={runId} file={file} />
            </DisclosurePanel>
          )}
        </>
      )}
    </Disclosure>
  );
}

function EarlierVersions({ runId, file }: { runId: string; file: ArtifactFile }) {
  return (
    <>
      {file.versions.map((version, index) =>
        index === 0 ? null : (
          <li
            key={`${version.stageId}#${version.retry}`}
            className="flex items-center gap-2 py-1.5 pr-3 pl-10 hover:bg-overlay sm:gap-4 sm:pr-4 sm:pl-14"
          >
            <span className="min-w-0 flex-1 truncate text-xs text-fg-3">
              <VersionLabel version={version} />
            </span>
            <span className="shrink-0 text-xs text-fg-muted tabular-nums">
              {formatBytes(version.size)}
            </span>
            <SizeDelta delta={version.delta} />
            <DownloadLink runId={runId} file={file} version={version} />
          </li>
        ),
      )}
    </>
  );
}

function VersionLabel({ version }: { version: ArtifactVersion }) {
  const attempt = attemptLabel(version);
  return (
    <>
      {version.stageLabel}
      {attempt && <span className="ml-2 text-fg-muted">{attempt}</span>}
    </>
  );
}

function attemptLabel(version: ArtifactVersion): string | null {
  return version.retry > 1 ? `attempt ${version.retry}` : null;
}

function SizeDelta({ delta }: { delta: number | null }) {
  if (delta === null) {
    return <span className="shrink-0 text-[11px] text-fg-muted tabular-nums">first</span>;
  }
  const tone = delta < 0 ? "text-amber" : "text-mint";
  const sign = delta < 0 ? "−" : "+";
  return (
    <span className={`shrink-0 text-[11px] ${tone} tabular-nums`}>
      {sign}
      {formatBytes(Math.abs(delta))}
    </span>
  );
}

function DownloadLink({
  runId,
  file,
  version,
}: {
  runId: string;
  file: ArtifactFile;
  version: ArtifactVersion;
}) {
  const href = stageArtifactDownloadUrl(
    runId,
    version.stageId,
    file.path,
    version.retry,
  );
  const attempt = attemptLabel(version);
  const source = attempt ? `${version.stageLabel}, ${attempt}` : version.stageLabel;
  return (
    <a
      href={href}
      download={file.name}
      aria-label={`Download ${file.name} from ${source}`}
      className="inline-flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-xs text-fg-3 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-current"
    >
      <ArrowDownTrayIcon className="size-3.5" aria-hidden="true" />
      <span className="hidden sm:inline">Download</span>
    </a>
  );
}

function errorMessage(error: unknown): string | undefined {
  return error instanceof Error ? error.message : undefined;
}
