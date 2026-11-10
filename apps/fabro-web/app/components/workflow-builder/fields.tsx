import { TrashIcon } from "@heroicons/react/16/solid";
import { useId, useState, type ReactNode } from "react";

import type { Attributes, FieldSpec } from "../../lib/workflow-builder/types";
import { ATTRIBUTE_KEY_RE } from "./graph-ops";

/** Fields whose values are identifiers, paths or code rather than prose. */
const TECHNICAL_FIELDS = new Set([
  "model",
  "provider",
  "default_model",
  "default_provider",
  "script",
  "language",
  "stdin_source",
  "model_stylesheet",
  "output_schema",
  "thread_id",
  "default_thread",
  "class",
  "timeout",
  "duration",
  "stall_timeout",
  "retry_target",
  "fallback_retry_target",
  "condition",
  "for_each",
  "import",
  "acp.command",
  "acp.config",
  "human.default_choice",
  "stack.child_workflow",
  "stack.child_dot_source",
  "manager.poll_interval",
  "manager.stop_condition",
]);

export function Section({
  title,
  action,
  children,
}: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="wfb-section">
      <div className="flex min-h-6 items-center justify-between gap-2">
        <h3 className="wfb-eyebrow">{title}</h3>
        {action}
      </div>
      <div className="mt-2 flex flex-col gap-3">{children}</div>
    </section>
  );
}

/** A collapsed section for settings most nodes never touch. */
export function Disclosure({
  title,
  count,
  defaultOpen = false,
  children,
}: {
  title: string;
  count?: number;
  defaultOpen?: boolean;
  children: ReactNode;
}) {
  return (
    <details className="wfb-disclosure" open={defaultOpen}>
      <summary className="wfb-eyebrow flex cursor-pointer items-center justify-between gap-2">
        <span>{title}</span>
        {count ? <span className="wfb-count">{count}</span> : null}
      </summary>
      <div className="mt-2 flex flex-col gap-3">{children}</div>
    </details>
  );
}

export function FieldShell({
  htmlFor,
  label,
  description,
  error,
  children,
}: {
  htmlFor?: string;
  label: string;
  description?: string;
  error?: string | null;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      {htmlFor ? (
        <label htmlFor={htmlFor} className="wfb-label">
          {label}
        </label>
      ) : (
        <span className="wfb-label">{label}</span>
      )}
      {children}
      {error ? (
        <p role="alert" className="wfb-field-error">
          {error}
        </p>
      ) : description ? (
        <p className="wfb-help">{description}</p>
      ) : null}
    </div>
  );
}

function inheritedPlaceholder(inherited: string | undefined): string | undefined {
  if (inherited === undefined) return undefined;
  const firstLine = inherited.split("\n")[0];
  return `Inherited: ${firstLine.length > 40 ? `${firstLine.slice(0, 40)}…` : firstLine}`;
}

/**
 * One attribute. An empty value removes the attribute from the source, so a
 * setting the author never chose doesn't get written out. A value the catalog
 * doesn't list stays selectable rather than being dropped.
 */
export function FieldControl({
  spec,
  value,
  inherited,
  disabled,
  onChange,
}: {
  spec: FieldSpec;
  value: string | undefined;
  inherited?: string;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  const id = useId();
  const current = value ?? "";
  const technical = TECHNICAL_FIELDS.has(spec.key);
  const inputClass = `wfb-input${technical ? " wfb-mono" : ""}`;
  const placeholder = inheritedPlaceholder(inherited);

  let control: ReactNode;
  if (spec.kind === "textarea") {
    control = (
      <textarea
        id={id}
        className={inputClass}
        rows={spec.key === "script" || spec.key === "model_stylesheet" ? 5 : 3}
        value={current}
        placeholder={placeholder}
        disabled={disabled}
        spellCheck={!technical}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  } else if (spec.kind === "select" || spec.kind === "boolean") {
    const options = spec.kind === "boolean" ? ["true", "false"] : (spec.options ?? []);
    const listed = current === "" || options.includes(current);
    control = (
      <select
        id={id}
        className={inputClass}
        value={current}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      >
        <option value="">{placeholder ?? "Not set"}</option>
        {options.map((option) => (
          <option key={option} value={option}>
            {option}
          </option>
        ))}
        {listed ? null : <option value={current}>{current} (as written)</option>}
      </select>
    );
  } else {
    const invalidNumber = spec.kind === "number" && current !== "" && !/^-?\d+$/.test(current);
    control = (
      <>
        <input
          id={id}
          type="text"
          inputMode={spec.kind === "number" ? "numeric" : undefined}
          className={`${inputClass}${spec.kind === "number" ? " wfb-mono" : ""}`}
          value={current}
          placeholder={placeholder}
          disabled={disabled}
          spellCheck={!technical}
          aria-invalid={invalidNumber || undefined}
          onChange={(event) => onChange(event.target.value)}
        />
        {invalidNumber ? <p className="wfb-field-warning">Fabro expects a whole number here.</p> : null}
      </>
    );
  }

  return (
    <FieldShell htmlFor={id} label={spec.label} description={spec.description}>
      {control}
    </FieldShell>
  );
}

/**
 * Attributes without a dedicated field. This is how future or unknown syntax
 * stays editable, and how nothing in the source is ever hidden from the author.
 */
export function AttributeTable({
  attrs,
  coveredKeys = [],
  disabled,
  label,
  onChange,
}: {
  attrs: Attributes;
  coveredKeys?: string[];
  disabled?: boolean;
  label: string;
  onChange: (next: Attributes) => void;
}) {
  const [newKey, setNewKey] = useState("");
  const [newValue, setNewValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const keyId = useId();
  const valueId = useId();
  const covered = new Set(coveredKeys);
  const rows = Object.entries(attrs).filter(([key]) => !covered.has(key));

  const add = () => {
    const key = newKey.trim();
    if (!ATTRIBUTE_KEY_RE.test(key)) {
      setError("Attribute names use letters, digits, dots, dashes and underscores.");
      return;
    }
    if (Object.hasOwn(attrs, key)) {
      setError(`${key} is already set. Edit it where it is listed.`);
      return;
    }
    onChange({ ...attrs, [key]: newValue });
    setNewKey("");
    setNewValue("");
    setError(null);
  };

  return (
    <div className="flex flex-col gap-2">
      {rows.map(([key, value]) => (
        <div key={key} className="flex flex-col gap-1">
          <div className="flex items-center justify-between gap-2">
            <span className="wfb-mono wfb-attr-key" title={key}>
              {key}
            </span>
            <button
              type="button"
              className="wfb-icon-button"
              aria-label={`Remove ${key}`}
              title={`Remove ${key}`}
              disabled={disabled}
              onClick={() => {
                const { [key]: _removed, ...rest } = attrs;
                onChange(rest);
              }}
            >
              <TrashIcon className="size-3" aria-hidden="true" />
            </button>
          </div>
          <textarea
            aria-label={`${key} value`}
            className="wfb-input wfb-mono"
            rows={value.includes("\n") ? 3 : 1}
            value={value}
            disabled={disabled}
            spellCheck={false}
            onChange={(event) => onChange({ ...attrs, [key]: event.target.value })}
          />
        </div>
      ))}
      <div className="wfb-attr-add">
        <div className="flex flex-col gap-1">
          <label htmlFor={keyId} className="wfb-label">
            {`New ${label}`}
          </label>
          <input
            id={keyId}
            className="wfb-input wfb-mono"
            placeholder="attribute_name"
            value={newKey}
            disabled={disabled}
            spellCheck={false}
            onChange={(event) => setNewKey(event.target.value)}
          />
        </div>
        <div className="flex flex-col gap-1">
          <label htmlFor={valueId} className="wfb-label">
            Value
          </label>
          <input
            id={valueId}
            className="wfb-input wfb-mono"
            value={newValue}
            disabled={disabled}
            spellCheck={false}
            onChange={(event) => setNewValue(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                add();
              }
            }}
          />
        </div>
        <button type="button" className="wfb-btn" disabled={disabled || !newKey.trim()} onClick={add}>
          Add
        </button>
      </div>
      {error ? (
        <p role="alert" className="wfb-field-error">
          {error}
        </p>
      ) : null}
    </div>
  );
}

/** An identifier edited as text and committed on blur or Enter; Escape puts it back. */
export function CommittedTextField({
  label,
  value,
  description,
  disabled,
  mono = true,
  validate,
  warn,
  onCommit,
}: {
  label: string;
  value: string;
  description?: string;
  disabled?: boolean;
  mono?: boolean;
  validate?: (next: string) => string | null;
  warn?: (next: string) => string | null;
  onCommit: (next: string) => void;
}) {
  const id = useId();
  // `null` means not editing, so the field follows the source until the author types.
  const [editing, setEditing] = useState<string | null>(null);
  const text = editing ?? value;
  const error = editing !== null && validate ? validate(editing) : null;
  const warning = !error && warn ? warn(text) : null;

  const commit = () => {
    if (editing === null) return;
    if (validate?.(editing)) return;
    if (editing !== value) onCommit(editing);
    setEditing(null);
  };

  return (
    <FieldShell htmlFor={id} label={label} description={warning ? undefined : description} error={error}>
      <input
        id={id}
        className={`wfb-input${mono ? " wfb-mono" : ""}`}
        value={text}
        disabled={disabled}
        spellCheck={false}
        aria-invalid={error ? true : undefined}
        onChange={(event) => setEditing(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            commit();
          } else if (event.key === "Escape") {
            setEditing(null);
          }
        }}
      />
      {warning ? <p className="wfb-field-warning">{warning}</p> : null}
    </FieldShell>
  );
}
