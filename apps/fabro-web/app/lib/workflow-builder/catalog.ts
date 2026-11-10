import type { FieldSpec, NodeKind } from "./types";

/**
 * What the visual editor offers. Attribute names come from Petri's lowering
 * tables (`crates/attractor/frontend/src/lower/attrs.rs`), which the workflow
 * engine actually enforces, cross-checked against docs/public/reference/dot-language.mdx.
 * Anything not listed stays editable as a generic attribute.
 */

const FIDELITY_OPTIONS = ["compact", "full", "summary:high", "summary:medium", "summary:low", "truncate"];
const FAILURE_POLICY_OPTIONS = ["route", "exit", "succeed"];

const PROMPT_FIELD: FieldSpec = {
  key: "prompt",
  label: "Prompt",
  kind: "textarea",
  description: "Task instructions. A value starting with @ loads that file, relative to the workflow file.",
};

const OUTPUT_SCHEMA_FIELD: FieldSpec = {
  key: "output_schema",
  label: "Output schema",
  kind: "text",
  description: "routing, @path/to/schema.json, or an inline JSON Schema object.",
};

const MODEL_FIELDS: FieldSpec[] = [
  { key: "model", label: "Model", kind: "text", description: "Explicit model ID. Overrides the stylesheet." },
  { key: "provider", label: "Provider", kind: "text", description: "Inferred from the model catalog when empty." },
  {
    key: "reasoning_effort",
    label: "Reasoning effort",
    kind: "select",
    options: ["low", "medium", "high", "xhigh", "max"],
    description: "Passed to the model as written. What a model accepts depends on the model.",
  },
  { key: "speed", label: "Speed", kind: "select", options: ["standard", "fast"] },
  { key: "max_tokens", label: "Max tokens", kind: "number" },
];

const CONTEXT_FIELDS: FieldSpec[] = [
  {
    key: "fidelity",
    label: "Fidelity",
    kind: "select",
    options: FIDELITY_OPTIONS,
    description: "How much prior context reaches this node.",
  },
  {
    key: "thread_id",
    label: "Thread",
    kind: "text",
    description: "Shared conversation thread. Requires fidelity=full.",
  },
  {
    key: "project_memory",
    label: "Project memory",
    kind: "boolean",
    description: "Include AGENTS.md, CLAUDE.md and similar project docs. On by default.",
  },
];

const OUTPUT_FIELDS: FieldSpec[] = [
  OUTPUT_SCHEMA_FIELD,
  {
    key: "output_retries",
    label: "Output repair turns",
    kind: "number",
    description: "Corrective turns when structured output fails validation. Default 2.",
  },
];

const LLM_FIELDS: FieldSpec[] = [PROMPT_FIELD, ...MODEL_FIELDS, ...CONTEXT_FIELDS, ...OUTPUT_FIELDS];

export const NODE_KINDS: NodeKind[] = [
  {
    shape: "Mdiamond",
    type: "start",
    label: "Start",
    description: "Workflow entry point. Exactly one is required.",
    defaults: { shape: "Mdiamond", label: "Start" },
    supportsPrompt: false,
    fields: [],
  },
  {
    shape: "Msquare",
    type: "exit",
    label: "Exit",
    description: "Workflow terminal. Exactly one is required.",
    defaults: { shape: "Msquare", label: "Exit" },
    supportsPrompt: false,
    fields: [],
  },
  {
    shape: "box",
    type: "agent",
    label: "Agent",
    description: "Multi-turn LLM with tool access.",
    defaults: { shape: "box", label: "Agent" },
    supportsPrompt: true,
    fields: [
      ...LLM_FIELDS,
      { key: "backend", label: "Backend", kind: "select", options: ["api", "acp"] },
      {
        key: "permissions",
        label: "Permissions",
        kind: "select",
        options: ["read-only", "read-write", "full"],
        description: "Petri extension. Fabro does not read it.",
      },
      {
        key: "acp.command",
        label: "ACP command",
        kind: "text",
        description: "Shell command for backend=acp. Excludes ACP config.",
      },
      {
        key: "acp.config",
        label: "ACP config",
        kind: "textarea",
        description: "JSON stdio config for backend=acp. Excludes ACP command.",
      },
    ],
  },
  {
    shape: "tab",
    type: "prompt",
    label: "Prompt",
    description: "Single LLM call with no tools.",
    defaults: { shape: "tab", label: "Prompt" },
    supportsPrompt: true,
    fields: LLM_FIELDS,
  },
  {
    shape: "parallelogram",
    type: "command",
    label: "Command",
    description: "Runs a shell or Python script in the sandbox.",
    defaults: { shape: "parallelogram", label: "Command" },
    supportsPrompt: false,
    fields: [
      { key: "script", label: "Script", kind: "textarea", description: "Required. The command to execute." },
      { key: "language", label: "Language", kind: "select", options: ["shell", "python"] },
      {
        key: "stdin_source",
        label: "Standard input source",
        kind: "text",
        description: "Run context key piped to stdin, such as context.parallel.results.",
      },
      OUTPUT_SCHEMA_FIELD,
    ],
  },
  {
    shape: "hexagon",
    type: "human",
    label: "Human",
    description: "Waits for a person to choose an outgoing edge.",
    defaults: { shape: "hexagon", label: "Review" },
    supportsPrompt: false,
    fields: [
      {
        key: "question_type",
        label: "Question type",
        kind: "select",
        options: ["yes_no", "confirmation", "multiple_choice", "multi_select", "freeform"],
      },
      {
        key: "review_target",
        label: "Review target",
        kind: "boolean",
        description: "Present the review_target context value as the primary link.",
      },
      {
        key: "human.default_choice",
        label: "Default choice on timeout",
        kind: "text",
        description: "Target node or accelerator key taken when the question times out.",
      },
      { key: "sensitive", label: "Sensitive answer", kind: "boolean", description: "Petri extension." },
    ],
  },
  {
    shape: "diamond",
    type: "conditional",
    label: "Conditional",
    description: "Routes on edge conditions. Runs nothing itself.",
    defaults: { shape: "diamond", label: "Decision" },
    supportsPrompt: false,
    fields: [],
  },
  {
    shape: "component",
    type: "parallel",
    label: "Parallel",
    description: "Fans out to concurrent branches and waits for all of them.",
    defaults: { shape: "component", label: "Fan out" },
    supportsPrompt: false,
    fields: [
      { key: "max_parallel", label: "Max parallel", kind: "number", description: "Concurrent branches. Default 4." },
      {
        key: "for_each",
        label: "For each",
        kind: "text",
        description: "Context key holding a JSON array. Needs exactly one agent or prompt target.",
      },
    ],
  },
  {
    shape: "tripleoctagon",
    type: "parallel.fan_in",
    label: "Fan-in",
    description: "Joins parallel branches. With a prompt, synthesizes their results.",
    defaults: { shape: "tripleoctagon", label: "Fan in" },
    supportsPrompt: true,
    fields: LLM_FIELDS,
  },
  {
    shape: "insulator",
    type: "wait",
    label: "Wait",
    description: "Pauses for a fixed duration.",
    defaults: { shape: "insulator", label: "Wait", duration: "30s" },
    supportsPrompt: false,
    fields: [
      { key: "duration", label: "Duration", kind: "text", description: "Required. Such as 500ms, 30s, 2m, 1h." },
    ],
  },
  {
    shape: "house",
    type: "stack.manager_loop",
    label: "Sub-workflow",
    description: "Runs a child workflow and supervises it.",
    defaults: { shape: "house", label: "Sub-workflow" },
    supportsPrompt: false,
    fields: [
      {
        key: "stack.child_workflow",
        label: "Child workflow path",
        kind: "text",
        description: "Path to the child .fabro file. Required unless inline source is set.",
      },
      {
        key: "stack.child_dot_source",
        label: "Inline child source",
        kind: "textarea",
        description: "Inline child workflow, as an alternative to the path.",
      },
      {
        key: "manager.poll_interval",
        label: "Poll interval",
        kind: "text",
        description: "How often to check the child. Default 45s.",
      },
      {
        key: "manager.max_cycles",
        label: "Max poll cycles",
        kind: "number",
        description: "Polls before the child is cancelled. Default 1000.",
      },
      {
        key: "manager.stop_condition",
        label: "Stop condition",
        kind: "text",
        description: "Condition that cancels the child early.",
      },
    ],
  },
];

export const GRAPH_FIELDS: FieldSpec[] = [
  { key: "goal", label: "Goal", kind: "textarea", description: "Workflow objective. @file loads it from a file." },
  { key: "label", label: "Label", kind: "text", description: "Display name." },
  { key: "rankdir", label: "Layout direction", kind: "select", options: ["TB", "LR", "BT", "RL"] },
  {
    key: "model_stylesheet",
    label: "Model stylesheet",
    kind: "textarea",
    description: "CSS-like rules assigning model, provider, reasoning_effort, speed and backend.",
  },
  { key: "default_model", label: "Default model", kind: "text" },
  { key: "default_provider", label: "Default provider", kind: "text" },
  { key: "backend", label: "Default backend", kind: "select", options: ["api", "acp"] },
  { key: "default_max_retries", label: "Default max retries", kind: "number" },
  {
    key: "on_failure",
    label: "On failure",
    kind: "select",
    options: FAILURE_POLICY_OPTIONS,
    description: "Failed-node policy when no explicit route matches. Default route.",
  },
  {
    key: "on_retries_exhausted",
    label: "On retries exhausted",
    kind: "select",
    options: [...FAILURE_POLICY_OPTIONS, "partially_succeed"],
    description: "Petri setting. partially_succeed is refused by Fabro's validator.",
  },
  { key: "retry_target", label: "Retry target", kind: "text", description: "Default node to jump to on retry." },
  { key: "fallback_retry_target", label: "Fallback retry target", kind: "text" },
  { key: "default_fidelity", label: "Default fidelity", kind: "select", options: FIDELITY_OPTIONS },
  { key: "default_thread", label: "Default thread", kind: "text" },
  {
    key: "max_node_visits",
    label: "Max node visits",
    kind: "number",
    description: "Visits per node across the run. 0 is unlimited; the engine caps it at 500.",
  },
  { key: "selection", label: "Edge selection", kind: "select", options: ["deterministic", "random"] },
  {
    key: "stall_timeout",
    label: "Stall timeout",
    kind: "text",
    description: "Duration before a stalled run is stopped. Default 1800s; 0s disables it.",
  },
  {
    key: "loop_restart_signature_limit",
    label: "Loop restart signature limit",
    kind: "number",
    description: "Repeats of one failure signature before aborting. Default 3.",
  },
  { key: "acp.command", label: "ACP command", kind: "text" },
  { key: "acp.config", label: "ACP config", kind: "textarea" },
  {
    key: "default_permissions",
    label: "Default permissions",
    kind: "select",
    options: ["read-only", "read-write", "full"],
    description: "Petri extension. Fabro does not read it.",
  },
];

/** Settings every node kind can carry; `NodeKind.fields` adds the kind-specific ones. */
export const NODE_FIELDS: FieldSpec[] = [
  { key: "label", label: "Label", kind: "text", description: "Display name." },
  {
    key: "shape",
    label: "Shape",
    kind: "select",
    options: NODE_KINDS.map((kind) => kind.shape),
    description: "Selects the handler unless Type is set.",
  },
  {
    key: "type",
    label: "Type",
    kind: "select",
    options: NODE_KINDS.map((kind) => kind.type),
    description: "Explicit handler. Overrides the shape.",
  },
  { key: "class", label: "Classes", kind: "text", description: "Stylesheet classes, separated by spaces." },
  { key: "timeout", label: "Timeout", kind: "text", description: "Needs a unit, such as 900s." },
  {
    key: "max_visits",
    label: "Max visits",
    kind: "number",
    description: "Runs of this node per workflow run. Overrides Max node visits.",
  },
  { key: "max_retries", label: "Max retries", kind: "number" },
  {
    key: "retry_policy",
    label: "Retry policy",
    kind: "select",
    options: ["none", "standard", "aggressive", "linear", "patient"],
  },
  { key: "retry_target", label: "Retry target", kind: "text", description: "Node to jump to on retry." },
  { key: "fallback_retry_target", label: "Fallback retry target", kind: "text" },
  {
    key: "on_failure",
    label: "On failure",
    kind: "select",
    options: FAILURE_POLICY_OPTIONS,
    description: "Overrides the graph's failure policy.",
  },
  {
    key: "on_retries_exhausted",
    label: "On retries exhausted",
    kind: "select",
    options: [...FAILURE_POLICY_OPTIONS, "partially_succeed"],
    description: "Petri setting. partially_succeed is refused by Fabro's validator.",
  },
  {
    key: "goal_gate",
    label: "Goal gate",
    kind: "boolean",
    description: "Fails the workflow if this node did not succeed. Pair it with a retry target.",
  },
  {
    key: "allow_partial",
    label: "Allow partial",
    kind: "boolean",
    description: "Exhausted retries become partially_succeeded instead of failed.",
  },
  {
    key: "selection",
    label: "Edge selection",
    kind: "select",
    options: ["deterministic", "random"],
    description: "random cannot be combined with conditional edges.",
  },
  {
    key: "import",
    label: "Import workflow",
    kind: "text",
    description: "Replaces this node with another .fabro file. Allows only class and inheritable model settings.",
  },
];

export const EDGE_FIELDS: FieldSpec[] = [
  { key: "label", label: "Label", kind: "text", description: "Display text. Human gates match their choices on it." },
  {
    key: "condition",
    label: "Condition",
    kind: "text",
    description: "Boolean expression, such as outcome=succeeded. Empty means unconditional.",
  },
  { key: "weight", label: "Weight", kind: "number", description: "Tiebreak priority. Higher wins." },
  { key: "fidelity", label: "Fidelity", kind: "select", options: FIDELITY_OPTIONS },
  { key: "thread_id", label: "Thread", kind: "text" },
  {
    key: "loop_restart",
    label: "Loop restart",
    kind: "boolean",
    description: "Restarts the workflow from the target when taken.",
  },
  {
    key: "freeform",
    label: "Freeform",
    kind: "boolean",
    description: "Human gate only: accept free text on this edge.",
  },
  { key: "human.description", label: "Choice description", kind: "text", description: "Petri extension." },
  { key: "human.preview", label: "Choice preview", kind: "text", description: "Petri extension." },
];
