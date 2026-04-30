import { useMemo, useState } from "react";
import { useParams } from "react-router";
import { CollapsibleFile } from "../components/collapsible-file";
import { StageSidebar } from "../components/stage-sidebar";
import {
  Badge,
  Count,
  Mono,
  Muted,
  Panel,
  PanelSkeleton,
  Row,
  type SettingsView,
  Toggle,
  ViewToggle,
} from "../components/settings-panel";
import { useRunSettings, useRunStages } from "../lib/queries";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";
import {
  type Snapshot,
  type SnapshotObject,
  getArray,
  getBool,
  getObject,
  getString,
  objectKeyCount,
} from "../lib/run-settings-snapshot";

export const handle = { wide: true };

export default function RunSettingsPage() {
  const { id } = useParams();
  const stagesQuery = useRunStages(id);
  const settingsQuery = useRunSettings<Snapshot>(id);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );
  const [view, setView] = useState<SettingsView>("settings");
  const snapshot = settingsQuery.data;

  return (
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id!} activeLink="settings" />

      <div className="min-w-0 flex-1 space-y-6">
        <PageIntro view={view} setView={setView} />

        {!snapshot ? (
          <>
            <PanelSkeleton />
            <PanelSkeleton />
            <PanelSkeleton />
            <PanelSkeleton />
          </>
        ) : view === "settings" ? (
          <>
            <WorkflowPanel snapshot={snapshot} />
            <SandboxPanel snapshot={snapshot} />
            <GitPanel snapshot={snapshot} />
            <ArtifactsPanel snapshot={snapshot} />
          </>
        ) : (
          <CollapsibleFile
            file={{
              name: "settings.json",
              contents: JSON.stringify(snapshot, null, 2),
              lang: "json",
            }}
          />
        )}
      </div>
    </div>
  );
}

function PageIntro({
  view,
  setView,
}: {
  view: SettingsView;
  setView: (v: SettingsView) => void;
}) {
  return (
    <div className="flex items-start justify-between gap-6">
      <div>
        <h2 className="text-base font-semibold text-fg">Run settings</h2>
        <p className="mt-1 text-sm/6 text-fg-3">
          Frozen settings snapshot used by this run.
        </p>
      </div>
      <ViewToggle view={view} setView={setView} />
    </div>
  );
}

function WorkflowPanel({ snapshot }: { snapshot: Snapshot }) {
  const workflow = getObject(snapshot, "workflow");
  const run = getObject(snapshot, "run");
  const name = getString(workflow, "name");
  const description = getString(workflow, "description");
  const goal = getObject(run, "goal");
  return (
    <Panel title="Workflow">
      <Row title="Name" help="Workflow identifier used by this run.">
        {name ? <Mono>{name}</Mono> : <Muted>Unnamed</Muted>}
      </Row>
      {description ? (
        <Row title="Description">
          <span className="text-fg-2">{description}</span>
        </Row>
      ) : null}
      <Row title="Goal" help="Goal text or file used for this run.">
        <GoalValue goal={goal} />
      </Row>
      <Row title="Inputs" help="Run input variables.">
        <Count
          n={objectKeyCount(run, "inputs")}
          singular="value"
          plural="values"
        />
      </Row>
      <Row title="Metadata" help="Combined run / workflow / project labels.">
        <Count
          n={
            objectKeyCount(run, "metadata") +
            objectKeyCount(workflow, "metadata") +
            objectKeyCount(getObject(snapshot, "project"), "metadata")
          }
          singular="label"
          plural="labels"
        />
      </Row>
    </Panel>
  );
}

function SandboxPanel({ snapshot }: { snapshot: Snapshot }) {
  const sandbox = getObject(getObject(snapshot, "run"), "sandbox");
  const provider = getString(sandbox, "provider");
  const docker = getObject(sandbox, "docker");
  const dockerImage = getString(docker, "image");
  const local = getObject(sandbox, "local");
  const worktreeMode = getString(local, "worktree_mode");
  return (
    <Panel title="Sandbox">
      <Row title="Provider" help="Execution environment for this run.">
        {provider ? <Badge>{provider}</Badge> : <Muted>Unknown</Muted>}
      </Row>
      {provider === "docker" && dockerImage ? (
        <Row title="Image" help="Docker image used for the sandbox.">
          <Mono>{dockerImage}</Mono>
        </Row>
      ) : null}
      {provider === "local" && worktreeMode ? (
        <Row title="Worktree mode" help="How the local provider materializes the workspace.">
          <Badge>{worktreeMode}</Badge>
        </Row>
      ) : null}
      <Row title="Devcontainer" help="Whether .devcontainer setup is honored.">
        <Toggle on={getBool(sandbox, "devcontainer") ?? false} />
      </Row>
      <Row title="Preserve" help="Keep the sandbox after the run completes.">
        <Toggle on={getBool(sandbox, "preserve") ?? false} />
      </Row>
      <Row title="Env" help="Environment variables injected into the sandbox.">
        <Count n={objectKeyCount(sandbox, "env")} singular="var" plural="vars" />
      </Row>
    </Panel>
  );
}

function GitPanel({ snapshot }: { snapshot: Snapshot }) {
  const run = getObject(snapshot, "run");
  const author = getObject(getObject(run, "git"), "author");
  const authorName = getString(author, "name");
  const authorEmail = getString(author, "email");
  const scm = getObject(run, "scm");
  const scmProvider = getString(scm, "provider");
  const scmOwner = getString(scm, "owner");
  const scmRepo = getString(scm, "repository");
  const repoLabel = scmOwner && scmRepo ? `${scmOwner}/${scmRepo}` : scmRepo ?? scmOwner;
  const pr = getObject(run, "pull_request");
  return (
    <Panel title="Git">
      <Row title="Author" help="Identity used for commits made during the run.">
        <AuthorValue name={authorName} email={authorEmail} />
      </Row>
      <Row title="SCM" help="Source-control provider and repository.">
        {scmProvider ? (
          <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
            <Badge>{scmProvider}</Badge>
            {repoLabel ? <Mono>{repoLabel}</Mono> : null}
          </span>
        ) : (
          <Muted>None</Muted>
        )}
      </Row>
      <Row title="Pull request" help="Whether this run opens a PR on completion.">
        <PullRequestValue pr={pr} />
      </Row>
    </Panel>
  );
}

function ArtifactsPanel({ snapshot }: { snapshot: Snapshot }) {
  const artifacts = getObject(getObject(snapshot, "run"), "artifacts");
  const include = getArray(artifacts, "include");
  return (
    <Panel title="Artifacts">
      <Row title="Include" help="Globs collected from the sandbox at run end.">
        {include && include.length > 0 ? (
          <GlobList globs={include.filter((e): e is string => typeof e === "string")} />
        ) : (
          <Muted>None</Muted>
        )}
      </Row>
    </Panel>
  );
}

function GoalValue({ goal }: { goal: SnapshotObject | undefined }) {
  if (!goal) return <Muted>None</Muted>;
  const type = getString(goal, "type");
  const value = getString(goal, "value");
  if (!value) return <Muted>None</Muted>;
  return (
    <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
      {type ? <Badge>{type}</Badge> : null}
      <Mono>{value}</Mono>
    </span>
  );
}

function AuthorValue({ name, email }: { name?: string; email?: string }) {
  if (!name && !email) return <Muted>Default</Muted>;
  if (name && email) {
    return (
      <span className="text-fg-2">
        {name} <span className="text-fg-muted">&lt;{email}&gt;</span>
      </span>
    );
  }
  return <span className="text-fg-2">{name ?? email}</span>;
}

function PullRequestValue({ pr }: { pr: SnapshotObject | undefined }) {
  const enabled = getBool(pr, "enabled") ?? false;
  if (!enabled) return <Toggle on={false} />;
  const draft = getBool(pr, "draft") ?? false;
  const autoMerge = getBool(pr, "auto_merge") ?? false;
  const strategy = getString(pr, "merge_strategy");
  return (
    <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
      <Toggle on={true} />
      {draft ? <Badge>draft</Badge> : null}
      {autoMerge ? <Badge>auto-merge</Badge> : null}
      {strategy ? (
        <span className="font-mono text-xs text-fg-3">{strategy}</span>
      ) : null}
    </span>
  );
}

function GlobList({ globs }: { globs: string[] }) {
  return (
    <span className="inline-flex flex-wrap items-center gap-1.5">
      {globs.map((g) => (
        <Badge key={g}>{g}</Badge>
      ))}
    </span>
  );
}
