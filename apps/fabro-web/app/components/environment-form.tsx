import { Disclosure, DisclosureButton, DisclosurePanel, Switch } from "@headlessui/react";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import {
  EnvironmentApiDockerfileSourceInlineTypeEnum,
  EnvironmentNetworkMode,
  EnvironmentProvider,
} from "@qltysh/fabro-api-client";
import type {
  CreateEnvironmentRequest,
  Environment,
  EnvironmentApiImageSettings,
  EnvironmentLifecycleSettings,
  EnvironmentNetworkSettings,
  EnvironmentResourcesSettings,
  ReplaceEnvironmentRequest,
} from "@qltysh/fabro-api-client";

import { Label, Panel, Row } from "./settings-panel";
import { INPUT_CLASS } from "./ui";
import {
  KeyValueEditor,
  entriesFromMap,
  mapFromEntries,
  type KeyValueEntry,
} from "./key-value-editor";

// Providers a managed environment can be created with. `local` is a reserved,
// in-memory environment, never a managed-environment provider, so it is never
// offered. The provider is fixed at creation time and cannot be changed.
export const CREATABLE_PROVIDERS = [
  EnvironmentProvider.DOCKER,
  EnvironmentProvider.DAYTONA,
] as const;

// Parse the `provider` query param used by the create flow into a creatable
// provider, defaulting to Docker for anything unexpected.
export function parseCreatableProvider(value: string | null): EnvironmentProvider {
  return value === EnvironmentProvider.DAYTONA
    ? EnvironmentProvider.DAYTONA
    : EnvironmentProvider.DOCKER;
}

// Environment ids are server-managed file names: lowercase, digits, hyphens.
const ENVIRONMENT_ID_PATTERN = /^[a-z0-9][a-z0-9-]{0,62}$/;

// Resource sliders pick a concrete value within a fixed range. Memory and disk
// are expressed in whole GB; the wire format keeps the `GB` suffix string.
const CPU = { min: 1, max: 8, step: 1, default: 4 };
const MEMORY = { min: 1, max: 16, step: 1, default: 8 };
const DISK = { min: 1, max: 20, step: 1, default: 16 };

// An environment image comes from exactly one source: a prebuilt image
// reference or an inline Dockerfile. The form keeps both field values around so
// switching back and forth doesn't lose typed text, and this discriminator
// decides which one is shown, required, and sent.
type ImageSource = "image" | "dockerfile";

export interface EnvironmentFormValues {
  id: string;
  provider: EnvironmentProvider;
  imageSource: ImageSource;
  dockerRef: string;
  dockerfile: string;
  cpu: number;
  memory: number;
  disk: number;
  blockNetwork: boolean;
  preserve: boolean;
  stopOnTerminal: boolean;
  autoStop: string;
  // Labels are not editable in the web UI — they're managed through the REST
  // API only. The form carries the loaded value verbatim so saving an edited
  // environment preserves any API-set labels instead of clearing them.
  labels: { [key: string]: string };
  envVars: KeyValueEntry[];
}

export const EMPTY_ENVIRONMENT_FORM: EnvironmentFormValues = {
  id:             "",
  provider:       EnvironmentProvider.DOCKER,
  imageSource:    "image",
  dockerRef:      "",
  dockerfile:     "",
  cpu:            CPU.default,
  memory:         MEMORY.default,
  disk:           DISK.default,
  blockNetwork:   false,
  preserve:       false,
  stopOnTerminal: true,
  autoStop:       "",
  labels:         {},
  envVars:        [],
};

export function environmentToFormValues(environment: Environment): EnvironmentFormValues {
  return {
    id:             environment.id,
    provider:       environment.provider,
    imageSource:    environment.image.dockerfile ? "dockerfile" : "image",
    dockerRef:      environment.image.docker ?? "",
    dockerfile:     environment.image.dockerfile?.value ?? "",
    cpu:            clampGb(environment.resources.cpu, CPU),
    memory:         parseGb(environment.resources.memory, MEMORY),
    disk:           parseGb(environment.resources.disk, DISK),
    blockNetwork:   environment.network.mode === EnvironmentNetworkMode.BLOCK,
    preserve:       environment.lifecycle.preserve,
    stopOnTerminal: environment.lifecycle.stop_on_terminal,
    autoStop:       environment.lifecycle.auto_stop ?? "",
    labels:         environment.labels,
    envVars:        entriesFromMap(environment.env),
  };
}

export function isEnvironmentFormValid(values: EnvironmentFormValues): boolean {
  if (!ENVIRONMENT_ID_PATTERN.test(values.id.trim())) return false;
  return imageSourceValue(values).trim() !== "";
}

// The currently selected image source's text, used both for validation and to
// drive which field is rendered as required.
function imageSourceValue(values: EnvironmentFormValues): string {
  return values.imageSource === "dockerfile" ? values.dockerfile : values.dockerRef;
}

// The Advanced disclosure (Network + Lifecycle) starts open when any of its
// values deviate from the defaults, so editing an environment never hides
// settings the operator already configured.
function hasNonDefaultAdvanced(values: EnvironmentFormValues): boolean {
  return (
    values.blockNetwork !== EMPTY_ENVIRONMENT_FORM.blockNetwork ||
    values.preserve !== EMPTY_ENVIRONMENT_FORM.preserve ||
    values.stopOnTerminal !== EMPTY_ENVIRONMENT_FORM.stopOnTerminal ||
    values.autoStop.trim() !== ""
  );
}

export function createRequestFromForm(values: EnvironmentFormValues): CreateEnvironmentRequest {
  return { id: values.id.trim(), ...settingsFromForm(values) };
}

export function replaceRequestFromForm(values: EnvironmentFormValues): ReplaceEnvironmentRequest {
  return settingsFromForm(values);
}

function settingsFromForm(values: EnvironmentFormValues): ReplaceEnvironmentRequest {
  return {
    provider:  values.provider,
    image:     imageFromForm(values),
    resources: resourcesFromForm(values),
    network:   networkFromForm(values),
    lifecycle: lifecycleFromForm(values),
    labels:    values.labels,
    env:       mapFromEntries(values.envVars),
  };
}

function imageFromForm(values: EnvironmentFormValues): EnvironmentApiImageSettings {
  if (values.imageSource === "dockerfile") {
    return {
      docker: null,
      dockerfile: {
        type:  EnvironmentApiDockerfileSourceInlineTypeEnum.INLINE,
        value: values.dockerfile,
      },
    };
  }
  return {
    docker:     values.dockerRef.trim() || null,
    dockerfile: null,
  };
}

function resourcesFromForm(values: EnvironmentFormValues): EnvironmentResourcesSettings {
  return {
    cpu:    values.cpu,
    memory: `${values.memory}GB`,
    disk:   `${values.disk}GB`,
  };
}

interface ResourceRange {
  min: number;
  max: number;
  step: number;
  default: number;
}

// Snap a numeric value into the slider range, falling back to the default when
// the environment leaves the resource unset (provider default).
function clampGb(value: number | null, range: ResourceRange): number {
  if (value === null) return range.default;
  return Math.min(range.max, Math.max(range.min, Math.round(value)));
}

// Parse a size string ("16GB", "512MiB", or a bare integer interpreted as GB)
// into whole GB within the slider range. Existing values may use other units or
// fall outside the range, so the result is rounded and clamped.
function parseGb(value: string | null, range: ResourceRange): number {
  if (value === null) return range.default;
  const match = value.trim().match(/^([\d.]+)\s*([a-zA-Z]*)$/);
  if (!match) return range.default;
  const amount = Number(match[1]);
  if (!Number.isFinite(amount)) return range.default;
  const perGb: { [unit: string]: number } = {
    "": 1, g: 1, gb: 1, gib: 1,
    m: 1 / 1000, mb: 1 / 1000, mib: 1 / 1000,
    t: 1000, tb: 1000, tib: 1000,
  };
  const factor = perGb[match[2].toLowerCase()] ?? 1;
  return clampGb(amount * factor, range);
}

function networkFromForm(values: EnvironmentFormValues): EnvironmentNetworkSettings {
  return {
    mode:  values.blockNetwork ? EnvironmentNetworkMode.BLOCK : EnvironmentNetworkMode.ALLOW_ALL,
    allow: [],
  };
}

function lifecycleFromForm(values: EnvironmentFormValues): EnvironmentLifecycleSettings {
  return {
    preserve:         values.preserve,
    stop_on_terminal: values.stopOnTerminal,
    auto_stop:        values.autoStop.trim() || null,
  };
}

function parseImageSource(value: string): ImageSource {
  return value === "dockerfile" ? "dockerfile" : "image";
}

interface EnvironmentFormFieldsProps {
  values: EnvironmentFormValues;
  onChange: (values: EnvironmentFormValues) => void;
  lockId?: boolean;
}

export function EnvironmentFormFields({
  values,
  onChange,
  lockId = false,
}: EnvironmentFormFieldsProps) {
  function patch(partial: Partial<EnvironmentFormValues>) {
    onChange({ ...values, ...partial });
  }

  const idValid = ENVIRONMENT_ID_PATTERN.test(values.id.trim());

  return (
    <>
      <Panel title="General">
        <Row
          title={<Label required>ID</Label>}
          help="Lowercase identifier (letters, digits, hyphens). Runs select this environment by id. Cannot be changed after creation."
        >
          {lockId ? (
            <div className="font-mono text-sm text-fg">{values.id}</div>
          ) : (
            <input
              type="text"
              name="id"
              aria-label="Environment ID"
              value={values.id}
              onChange={(e) => patch({ id: e.target.value })}
              placeholder="fabro-dev"
              autoComplete="off"
              spellCheck={false}
              className={`${INPUT_CLASS} font-mono`}
            />
          )}
        </Row>
        <Row
          title={<Label required>Source</Label>}
          help="Whether this environment runs a prebuilt image reference or builds from an inline Dockerfile."
        >
          <select
            name="image_source"
            aria-label="Image source"
            value={values.imageSource}
            onChange={(e) => patch({ imageSource: parseImageSource(e.target.value) })}
            className={INPUT_CLASS}
          >
            <option value="image">Image reference</option>
            <option value="dockerfile">Dockerfile</option>
          </select>
        </Row>
        {values.imageSource === "image" ? (
          <Row
            title={<Label required>Image reference</Label>}
            help="Docker image or Daytona snapshot name (e.g. fabro-v11)."
          >
            <input
              type="text"
              name="docker_ref"
              aria-label="Image reference"
              value={values.dockerRef}
              onChange={(e) => patch({ dockerRef: e.target.value })}
              placeholder="ubuntu:24.04"
              autoComplete="off"
              spellCheck={false}
              className={`${INPUT_CLASS} font-mono`}
            />
          </Row>
        ) : (
          <Row
            title={<Label required>Dockerfile</Label>}
            help="Inline Dockerfile contents. The REST API accepts inline Dockerfiles only — local paths are rejected."
          >
            <textarea
              name="dockerfile"
              aria-label="Dockerfile"
              value={values.dockerfile}
              onChange={(e) => patch({ dockerfile: e.target.value })}
              rows={5}
              placeholder={"FROM ubuntu:24.04\nRUN apt-get update && apt-get install -y git"}
              autoComplete="off"
              spellCheck={false}
              className={`${INPUT_CLASS} resize-y font-mono`}
            />
          </Row>
        )}
      </Panel>

      <Panel title="Resources">
        <Row title="CPU" help="Number of vCPUs allocated to each run.">
          <ResourceSlider
            ariaLabel="CPU"
            range={CPU}
            value={values.cpu}
            onChange={(cpu) => patch({ cpu })}
            format={(n) => `${n} CPU`}
          />
        </Row>
        <Row title="Memory" help="Memory limit for each run.">
          <ResourceSlider
            ariaLabel="Memory"
            range={MEMORY}
            value={values.memory}
            onChange={(memory) => patch({ memory })}
            format={(n) => `${n} GB`}
          />
        </Row>
        <Row title="Disk" help="Disk limit for each run.">
          <ResourceSlider
            ariaLabel="Disk"
            range={DISK}
            value={values.disk}
            onChange={(disk) => patch({ disk })}
            format={(n) => `${n} GB`}
          />
        </Row>
      </Panel>

      <Panel title="Environment variables">
        <div className="px-4 py-3.5">
          <p className="mb-3 text-xs/5 text-fg-3">
            Variables injected into the sandbox for every run.
          </p>
          <KeyValueEditor
            entries={values.envVars}
            onChange={(envVars) => patch({ envVars })}
            keyPlaceholder="TZ"
            valuePlaceholder="UTC"
            addLabel="Add variable"
          />
        </div>
      </Panel>

      <Disclosure as="div" className="space-y-4" defaultOpen={hasNonDefaultAdvanced(values)}>
        <DisclosureButton className="group flex items-center gap-1.5 text-xs font-medium uppercase tracking-wider text-fg-muted transition-colors hover:text-fg-3 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500">
          <ChevronRightIcon
            className="size-3.5 transition-transform duration-150 group-data-open:rotate-90"
            aria-hidden="true"
          />
          Advanced
        </DisclosureButton>
        <DisclosurePanel className="space-y-6">
          <Panel title="Network">
            <Row
              title="Block all network access"
              help="Block all outbound network access from the sandbox."
            >
              <ToggleSwitch
                checked={values.blockNetwork}
                onChange={(blockNetwork) => patch({ blockNetwork })}
                label="Block all network access"
              />
            </Row>
          </Panel>

          <Panel title="Lifecycle">
            <Row title="Preserve" help="Keep the sandbox after the run finishes instead of tearing it down.">
              <ToggleSwitch
                checked={values.preserve}
                onChange={(preserve) => patch({ preserve })}
                label="Preserve sandbox after run"
              />
            </Row>
            <Row title="Stop on terminal" help="Stop the sandbox when the run reaches a terminal state.">
              <ToggleSwitch
                checked={values.stopOnTerminal}
                onChange={(stopOnTerminal) => patch({ stopOnTerminal })}
                label="Stop sandbox on terminal state"
              />
            </Row>
            <Row title={<Label optional>Auto-stop</Label>} help="Idle duration before the sandbox is stopped (e.g. 30m). Leave blank to disable.">
              <input
                type="text"
                name="auto_stop"
                aria-label="Auto-stop"
                value={values.autoStop}
                onChange={(e) => patch({ autoStop: e.target.value })}
                placeholder="30m"
                autoComplete="off"
                spellCheck={false}
                className={`${INPUT_CLASS} font-mono`}
              />
            </Row>
          </Panel>
        </DisclosurePanel>
      </Disclosure>

      {!lockId && values.id.trim() !== "" && !idValid ? (
        <p className="text-xs text-coral">
          ID must be lowercase letters, digits, or hyphens and start with a letter or digit.
        </p>
      ) : null}
    </>
  );
}

function ResourceSlider({
  value,
  range,
  ariaLabel,
  format,
  onChange,
}: {
  value: number;
  range: ResourceRange;
  ariaLabel: string;
  format: (value: number) => string;
  onChange: (value: number) => void;
}) {
  const fill = ((value - range.min) / (range.max - range.min)) * 100;
  return (
    <div className="flex items-center gap-4">
      <div className="relative h-4 flex-1">
        <div className="pointer-events-none absolute inset-x-0 top-1/2 h-1.5 -translate-y-1/2 rounded-full bg-overlay-strong">
          <div className="h-full rounded-full bg-teal-500" style={{ width: `${fill}%` }} />
        </div>
        <input
          type="range"
          aria-label={ariaLabel}
          value={value}
          min={range.min}
          max={range.max}
          step={range.step}
          onChange={(e) => onChange(Number(e.target.value))}
          className="relative h-4 w-full cursor-pointer appearance-none bg-transparent focus-visible:outline-none [&::-moz-range-thumb]:size-4 [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:border-0 [&::-moz-range-thumb]:bg-fg [&::-moz-range-thumb]:shadow-sm [&::-moz-range-track]:h-1.5 [&::-moz-range-track]:rounded-full [&::-moz-range-track]:bg-transparent [&::-webkit-slider-runnable-track]:h-1.5 [&::-webkit-slider-runnable-track]:rounded-full [&::-webkit-slider-runnable-track]:bg-transparent [&::-webkit-slider-thumb]:-mt-[5px] [&::-webkit-slider-thumb]:size-4 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-fg [&::-webkit-slider-thumb]:shadow-sm [&::-webkit-slider-thumb]:outline [&::-webkit-slider-thumb]:outline-1 [&::-webkit-slider-thumb]:-outline-offset-1 [&::-webkit-slider-thumb]:outline-line-strong focus-visible:[&::-webkit-slider-thumb]:outline-2 focus-visible:[&::-webkit-slider-thumb]:outline-teal-500"
        />
      </div>
      <output className="w-16 shrink-0 text-right font-mono text-sm tabular-nums text-fg">
        {format(value)}
      </output>
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
