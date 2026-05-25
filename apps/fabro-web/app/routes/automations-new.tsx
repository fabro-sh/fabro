import { useRef, useState, type ReactNode } from "react";
import { Link, useNavigate } from "react-router";
import { Switch } from "@headlessui/react";
import { ChevronRightIcon, ChevronDownIcon } from "@heroicons/react/20/solid";

import { Panel, Row } from "../components/settings-panel";
import {
  INPUT_CLASS,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";

export function meta() {
  return [{ title: "New automation — Fabro" }];
}

export const handle = { hideHeader: true };

const SAMPLE_REPOSITORIES = [
  "qltysh/fabro",
  "qltysh/fabro-cloud",
  "acme/orders-api",
  "acme/billing",
  "acme/web",
];

const CRON_PRESETS: ReadonlyArray<{ label: string; value: string }> = [
  { label: "Every hour",      value: "0 * * * *" },
  { label: "Daily 9:00 UTC",  value: "0 9 * * *" },
  { label: "Weekdays 9:00",   value: "0 9 * * 1-5" },
  { label: "Mondays 8:00",    value: "0 8 * * 1" },
];

export default function AutomationsNew() {
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const slugTouchedRef = useRef(false);
  const [description, setDescription] = useState("");
  const [repository, setRepository] = useState(SAMPLE_REPOSITORIES[0]);
  const [branch, setBranch] = useState("main");
  const [workflowSlug, setWorkflowSlug] = useState("");
  const [goal, setGoal] = useState("");
  const [manualEnabled, setManualEnabled] = useState(true);
  const [scheduleEnabled, setScheduleEnabled] = useState(false);
  const [cron, setCron] = useState("0 9 * * 1-5");

  function onNameChange(next: string) {
    setName(next);
    if (!slugTouchedRef.current) setSlug(kebabify(next));
  }

  function onSlugChange(next: string) {
    setSlug(kebabify(next));
    slugTouchedRef.current = true;
  }

  const canSubmit =
    name.trim() !== "" &&
    slug.trim() !== "" &&
    workflowSlug.trim() !== "" &&
    (manualEnabled || scheduleEnabled);

  function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    // Non-functional prototype — just return to the list.
    navigate("/automations");
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      <PageHeader />

      <Panel title="Basics">
        <Row title={<Label required>Name</Label>} help="Shown wherever this automation is listed.">
          <input
            type="text"
            name="name"
            aria-label="Automation name"
            value={name}
            onChange={(e) => onNameChange(e.target.value)}
            placeholder="Fix Build"
            autoComplete="off"
            className={INPUT_CLASS}
          />
        </Row>
        <Row
          title={<Label required>Slug</Label>}
          help={
            <>
              Identifier used in the URL: <span className="font-mono text-fg-2">/automation/{slug || "<slug>"}</span>
            </>
          }
        >
          <input
            type="text"
            name="slug"
            aria-label="Automation slug"
            value={slug}
            onChange={(e) => onSlugChange(e.target.value)}
            placeholder="fix-build"
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
        <Row title={<Label optional>Description</Label>} help="A short summary teammates will see when browsing automations.">
          <textarea
            name="description"
            aria-label="Automation description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={2}
            placeholder="Diagnose and fix CI build failures by analyzing logs and applying targeted patches."
            className={`${INPUT_CLASS} resize-y`}
          />
        </Row>
      </Panel>

      <Panel title="Source">
        <Row title={<Label required>Repository</Label>} help="The repository this automation will check out and operate on.">
          <SelectInput
            name="repository"
            label="Repository"
            value={repository}
            onChange={setRepository}
            options={SAMPLE_REPOSITORIES}
          />
        </Row>
        <Row title={<Label required>Branch</Label>} help="Default branch to run against.">
          <input
            type="text"
            name="branch"
            aria-label="Default branch"
            value={branch}
            onChange={(e) => setBranch(e.target.value)}
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
            value={workflowSlug}
            onChange={(e) => setWorkflowSlug(snakeify(e.target.value))}
            placeholder="fix_build"
            autoComplete="off"
            spellCheck={false}
            className={`${INPUT_CLASS} font-mono`}
          />
        </Row>
      </Panel>

      <Panel title="Goal">
        <Row title={<Label optional>Goal</Label>} help="Plain-English objective the agent uses to decide when the workflow has succeeded.">
          <textarea
            name="goal"
            aria-label="Automation goal"
            value={goal}
            onChange={(e) => setGoal(e.target.value)}
            rows={3}
            placeholder="Diagnose and fix the failing build so CI is green on the target branch."
            className={`${INPUT_CLASS} resize-y`}
          />
        </Row>
      </Panel>

      <Panel title="Triggers">
        <Row title="Manual / API" help="Start a run by clicking Run in the UI or calling the API.">
          <ToggleSwitch
            checked={manualEnabled}
            onChange={setManualEnabled}
            label="Enable manual and API triggers"
          />
        </Row>
        <Row title="Schedule" help="Start runs automatically on a recurring cron schedule.">
          <ToggleSwitch
            checked={scheduleEnabled}
            onChange={setScheduleEnabled}
            label="Enable scheduled triggers"
          />
        </Row>
        {scheduleEnabled ? (
          <Row
            title="Cron expression"
            help={
              <>
                Five-field POSIX cron in UTC. Next run: <span className="text-fg-2">{describeCron(cron)}</span>
              </>
            }
          >
            <div className="space-y-2">
              <input
                type="text"
                name="cron"
                aria-label="Cron expression"
                value={cron}
                onChange={(e) => setCron(e.target.value)}
                placeholder="0 9 * * 1-5"
                autoComplete="off"
                spellCheck={false}
                className={`${INPUT_CLASS} font-mono`}
              />
              <div className="flex flex-wrap gap-1.5">
                {CRON_PRESETS.map((preset) => {
                  const active = preset.value === cron;
                  return (
                    <button
                      key={preset.value}
                      type="button"
                      onClick={() => setCron(preset.value)}
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

      <FormFooter canSubmit={canSubmit} onCancel={() => navigate("/automations")} />
    </form>
  );
}

function PageHeader() {
  return (
    <div>
      <nav className="mb-4 flex items-center gap-1 text-sm text-fg-muted">
        <Link to="/automations" className="text-fg-3 hover:text-fg">
          Automations
        </Link>
        <ChevronRightIcon className="size-3" aria-hidden="true" />
        <span>New automation</span>
      </nav>
      <h2 className="text-xl font-semibold text-fg">New automation</h2>
      <p className="mt-2 max-w-prose text-sm leading-relaxed text-fg-3">
        Define a workflow that Fabro can run on demand, on a schedule, or via the API.
        You can refine the graph and per-stage prompts after it's created.
      </p>
    </div>
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

function SelectInput({
  name,
  label,
  value,
  onChange,
  options,
}: {
  name: string;
  label: string;
  value: string;
  onChange: (next: string) => void;
  options: ReadonlyArray<string>;
}) {
  return (
    <div className="relative">
      <select
        name={name}
        aria-label={label}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={`${INPUT_CLASS} appearance-none pr-9 font-mono`}
      >
        {options.map((option) => (
          <option key={option} value={option}>
            {option}
          </option>
        ))}
      </select>
      <ChevronDownIcon
        className="pointer-events-none absolute right-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted"
        aria-hidden="true"
      />
    </div>
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

function FormFooter({
  canSubmit,
  onCancel,
}: {
  canSubmit: boolean;
  onCancel: () => void;
}) {
  return (
    <div className="flex items-center justify-end gap-3 pt-2">
      <button type="button" onClick={onCancel} className={SECONDARY_BUTTON_CLASS}>
        Cancel
      </button>
      <button type="submit" disabled={!canSubmit} className={PRIMARY_BUTTON_CLASS}>
        Create automation
      </button>
    </div>
  );
}

function kebabify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}

function snakeify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_|_$/g, "");
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
