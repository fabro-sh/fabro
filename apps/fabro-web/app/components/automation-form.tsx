import { useRef, type ReactNode } from "react";
import { Switch } from "@headlessui/react";
import type {
  Automation,
  AutomationTrigger,
  Run,
  WorkflowSettings,
} from "@qltysh/fabro-api-client";

import { Panel, Row } from "./settings-panel";
import { INPUT_CLASS } from "./ui";
import { sandboxRuntime } from "../lib/run-sandbox-lifecycle";

export interface AutomationFormValues {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  repository: string;
  ref: string;
  workflow: string;
  manualEnabled: boolean;
  scheduleEnabled: boolean;
  cron: string;
}

export const EMPTY_AUTOMATION_FORM: AutomationFormValues = {
  id:              "",
  name:            "",
  description:     "",
  enabled:         true,
  repository:      "",
  ref:             "main",
  workflow:        "",
  manualEnabled:   true,
  scheduleEnabled: false,
  cron:            "0 9 * * 1-5",
};

const CRON_PRESETS: ReadonlyArray<{ label: string; value: string }> = [
  { label: "Every hour",      value: "0 * * * *" },
  { label: "Daily 9:00 UTC",  value: "0 9 * * *" },
  { label: "Weekdays 9:00",   value: "0 9 * * 1-5" },
  { label: "Mondays 8:00",    value: "0 8 * * 1" },
];

export function automationToFormValues(automation: Automation): AutomationFormValues {
  const apiTrigger = automation.triggers.find((t) => t.type === "api");
  const scheduleTrigger = automation.triggers.find((t) => t.type === "schedule");
  return {
    id:              automation.id,
    name:            automation.name,
    description:     automation.description ?? "",
    enabled:         automation.enabled,
    repository:      automation.target.repository,
    ref:             automation.target.ref,
    workflow:        automation.target.workflow,
    manualEnabled:   apiTrigger?.enabled ?? false,
    scheduleEnabled: scheduleTrigger?.enabled ?? false,
    cron:            scheduleTrigger?.expression ?? "0 9 * * 1-5",
  };
}

export function automationFormValuesFromRun(
  run: Run,
  settings?: WorkflowSettings | null,
): AutomationFormValues {
  const name = firstPresentString(
    run.title,
    run.workflow.name,
    run.workflow.graph_name,
    run.workflow.slug,
    "New automation",
  );
  const workflowName = firstPresentString(
    run.workflow.name,
    run.workflow.graph_name,
    name,
  );
  const repository = githubRepositoryFromSettings(settings)
    ?? githubRepositoryName(run.repository?.name)
    ?? githubRepositoryFromOriginUrl(run.repository?.origin_url)
    ?? "";
  const cloneBranch = sandboxRuntime(run.sandbox)?.clone_branch;
  return {
    ...EMPTY_AUTOMATION_FORM,
    id:         kebabify(name),
    name,
    repository,
    ref:        cloneBranch ?? EMPTY_AUTOMATION_FORM.ref,
    workflow:   run.workflow.slug?.trim() || snakeify(workflowName),
  };
}

export function triggersFromFormValues(values: AutomationFormValues): AutomationTrigger[] {
  const triggers: AutomationTrigger[] = [];
  if (values.manualEnabled) {
    triggers.push({ id: "manual", type: "api", enabled: true });
  }
  if (values.scheduleEnabled) {
    triggers.push({
      id:         "schedule",
      type:       "schedule",
      enabled:    true,
      expression: values.cron.trim(),
    });
  }
  return triggers;
}

export function isFormValid(values: AutomationFormValues): boolean {
  return (
    values.id.trim() !== "" &&
    values.name.trim() !== "" &&
    values.repository.trim() !== "" &&
    values.ref.trim() !== "" &&
    values.workflow.trim() !== "" &&
    (values.manualEnabled || values.scheduleEnabled)
  );
}

export function kebabify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}

export function snakeify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_|_$/g, "");
}

function firstPresentString(...values: Array<string | null | undefined>): string {
  for (const value of values) {
    const trimmed = value?.trim();
    if (trimmed) return trimmed;
  }
  return "";
}

function githubRepositoryFromSettings(
  settings?: WorkflowSettings | null,
): string | null {
  const owner = settings?.run?.scm?.owner;
  const repository = settings?.run?.scm?.repository;
  if (!owner || !repository) return null;
  return githubRepositoryName(`${owner}/${repository}`);
}

function githubRepositoryName(value: string | null | undefined): string | null {
  const trimmed = value?.trim().replace(/\.git$/i, "");
  if (!trimmed) return null;

  const match = trimmed.match(/^([A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)\/([A-Za-z0-9._-]+)$/);
  if (!match) return null;
  return `${match[1]}/${match[2]}`;
}

function githubRepositoryFromOriginUrl(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  if (!trimmed) return null;

  const scpLikeMatch = trimmed.match(
    /^git@github\.com:([A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)\/([A-Za-z0-9._-]+?)(?:\.git)?$/i,
  );
  if (scpLikeMatch) {
    return githubRepositoryName(`${scpLikeMatch[1]}/${scpLikeMatch[2]}`);
  }

  try {
    const url = new URL(trimmed);
    if (url.hostname.toLowerCase() !== "github.com") return null;
    const parts = url.pathname.split("/").filter(Boolean);
    if (parts.length !== 2) return null;
    return githubRepositoryName(`${parts[0]}/${parts[1]}`);
  } catch {
    return null;
  }
}

function describeCron(expression: string): string {
  const trimmed = expression.trim();
  const preset = CRON_PRESETS.find((p) => p.value === trimmed);
  if (preset) return preset.label;
  if (!/^[\d*/,\-\s]+$/.test(trimmed) || trimmed.split(/\s+/).length !== 5) {
    return "Waiting for a valid expression…";
  }
  return "Computed when saved";
}

interface AutomationFormFieldsProps {
  values: AutomationFormValues;
  onChange: (values: AutomationFormValues) => void;
  lockIdAndTarget?: boolean;
}

export function AutomationFormFields({
  values,
  onChange,
  lockIdAndTarget = false,
}: AutomationFormFieldsProps) {
  const slugTouchedRef = useRef(values.id.length > 0);

  function patch(partial: Partial<AutomationFormValues>) {
    onChange({ ...values, ...partial });
  }

  function onNameChange(next: string) {
    if (slugTouchedRef.current || lockIdAndTarget) {
      patch({ name: next });
    } else {
      patch({ name: next, id: kebabify(next) });
    }
  }

  function onSlugChange(next: string) {
    slugTouchedRef.current = true;
    patch({ id: kebabify(next) });
  }

  return (
    <>
      <Panel title="Basics">
        <Row title={<Label required>Name</Label>} help="Shown wherever this automation is listed.">
          <input
            type="text"
            name="name"
            aria-label="Automation name"
            value={values.name}
            onChange={(e) => onNameChange(e.target.value)}
            placeholder="Fix Build"
            autoComplete="off"
            className={INPUT_CLASS}
          />
        </Row>
        {lockIdAndTarget ? null : (
          <Row
            title={<Label required>Slug</Label>}
            help={
              <>
                Identifier used in the URL:{" "}
                <span className="font-mono text-fg-2">/automations/{values.id || "<slug>"}</span>
              </>
            }
          >
            <input
              type="text"
              name="slug"
              aria-label="Automation slug"
              value={values.id}
              onChange={(e) => onSlugChange(e.target.value)}
              placeholder="fix-build"
              autoComplete="off"
              spellCheck={false}
              className={`${INPUT_CLASS} font-mono`}
            />
          </Row>
        )}
        <Row title={<Label optional>Description</Label>} help="A short summary teammates will see when browsing automations.">
          <textarea
            name="description"
            aria-label="Automation description"
            value={values.description}
            onChange={(e) => patch({ description: e.target.value })}
            rows={2}
            placeholder="Diagnose and fix CI build failures by analyzing logs and applying targeted patches."
            className={`${INPUT_CLASS} resize-y`}
          />
        </Row>
        <Row title="Enabled" help="Disabled automations skip scheduled triggers and reject API runs.">
          <ToggleSwitch
            checked={values.enabled}
            onChange={(enabled) => patch({ enabled })}
            label="Enable automation"
          />
        </Row>
      </Panel>

      <Panel title="Source">
        <Row title={<Label required>Repository</Label>} help="GitHub repository in owner/repo form.">
          <input
            type="text"
            name="repository"
            aria-label="Repository"
            value={values.repository}
            onChange={(e) => patch({ repository: e.target.value })}
            placeholder="acme/orders-api"
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
        <Row title={<Label required>Branch</Label>} help="Default branch to run against.">
          <input
            type="text"
            name="branch"
            aria-label="Default branch"
            value={values.ref}
            onChange={(e) => patch({ ref: e.target.value })}
            placeholder="main"
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
        <Row
          title={<Label required>Workflow slug</Label>}
          help="Snake-case identifier used in the workflow file name (e.g. fix_build.fabro)."
        >
          <input
            type="text"
            name="workflow_slug"
            aria-label="Workflow slug"
            value={values.workflow}
            onChange={(e) => patch({ workflow: snakeify(e.target.value) })}
            placeholder="fix_build"
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
      </Panel>

      <Panel title="Triggers">
        <Row title="Manual / API" help="Start a run by clicking Run in the UI or calling the API.">
          <ToggleSwitch
            checked={values.manualEnabled}
            onChange={(manualEnabled) => patch({ manualEnabled })}
            label="Enable manual and API triggers"
          />
        </Row>
        <Row title="Schedule" help="Start runs automatically on a recurring cron schedule.">
          <ToggleSwitch
            checked={values.scheduleEnabled}
            onChange={(scheduleEnabled) => patch({ scheduleEnabled })}
            label="Enable scheduled triggers"
          />
        </Row>
        {values.scheduleEnabled ? (
          <Row
            title="Cron expression"
            help={
              <>
                Five-field POSIX cron in UTC. Next run:{" "}
                <span className="text-fg-2">{describeCron(values.cron)}</span>
              </>
            }
          >
            <div className="space-y-2">
              <input
                type="text"
                name="cron"
                aria-label="Cron expression"
                value={values.cron}
                onChange={(e) => patch({ cron: e.target.value })}
                placeholder="0 9 * * 1-5"
                autoComplete="off"
                spellCheck={false}
                className={`${INPUT_CLASS} font-mono`}
              />
              <div className="flex flex-wrap gap-1.5">
                {CRON_PRESETS.map((preset) => {
                  const active = preset.value === values.cron;
                  return (
                    <button
                      key={preset.value}
                      type="button"
                      onClick={() => patch({ cron: preset.value })}
                      aria-pressed={active}
                      className={`rounded-full px-2.5 py-1 text-xs transition-colors ${
                        active
                          ? "bg-teal-500/15 text-teal-300 outline-1 -outline-offset-1 outline-teal-500/40"
                          : "bg-overlay text-fg-3 hover:bg-overlay-strong hover:text-fg-2"
                      }`}
                    >
                      {preset.label}
                    </button>
                  );
                })}
              </div>
            </div>
          </Row>
        ) : null}
      </Panel>
    </>
  );
}

function Label({
  children,
  required,
  optional,
}: {
  children: ReactNode;
  required?: boolean;
  optional?: boolean;
}) {
  return (
    <span className="inline-flex items-baseline gap-1.5">
      <span>{children}</span>
      {required ? (
        <span aria-label="required" className="text-coral">
          *
        </span>
      ) : null}
      {optional ? <span className="text-xs font-normal text-fg-muted">Optional</span> : null}
    </span>
  );
}

function ToggleSwitch({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
}) {
  return (
    <Switch
      checked={checked}
      onChange={onChange}
      aria-label={label}
      className="group relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full bg-overlay-strong outline-1 -outline-offset-1 outline-line-strong transition-colors duration-150 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 data-checked:bg-teal-500"
    >
      <span className="pointer-events-none inline-block size-4 translate-x-0.5 rounded-full bg-fg shadow-sm transition-transform duration-150 group-data-checked:translate-x-[1.125rem]" />
    </Switch>
  );
}
