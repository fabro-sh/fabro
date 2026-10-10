import type {
  Automation,
  AutomationGitWorkflowSource,
  AutomationTrigger,
  RunTarget,
} from "@qltysh/fabro-api-client";

export type GitRunTarget = Extract<RunTarget, { kind: "git" }>;

/** Label shown in place of a repository when an automation's target is not Git-backed. */
export const UNSUPPORTED_TARGET_LABEL = "Unsupported target";

export function gitTarget(target: RunTarget | null | undefined): GitRunTarget | null {
  return target?.kind === "git" ? target : null;
}

type TriggerOfType<K extends AutomationTrigger["type"]> = Extract<
  AutomationTrigger,
  { type: K }
>;

export function findApiTrigger(automation: Automation): TriggerOfType<"api"> | undefined {
  return automation.triggers.find((t): t is TriggerOfType<"api"> => t.type === "api");
}

export function findScheduleTrigger(
  automation: Automation,
): TriggerOfType<"schedule"> | undefined {
  return automation.triggers.find((t): t is TriggerOfType<"schedule"> => t.type === "schedule");
}

export function findScheduleTriggers(automation: Automation): TriggerOfType<"schedule">[] {
  return automation.triggers.filter((t): t is TriggerOfType<"schedule"> => t.type === "schedule");
}

export function hasEnabledApiTrigger(automation: Automation): boolean {
  return findApiTrigger(automation)?.enabled === true;
}

export function workflowSourceSummary(source: AutomationGitWorkflowSource): string {
  if (source.sha) return `${source.repo} · commit ${source.sha}`;
  if (source.tag) return `${source.repo} · tag ${source.tag}`;
  return `${source.repo} · branch ${source.branch}`;
}

export const RUN_TARGET_CHECKOUT_LABEL = "run target checkout";

/** Where an automation's workflow files come from, for display. */
export function workflowSourceLabel(source: AutomationGitWorkflowSource | undefined): string {
  return source ? workflowSourceSummary(source) : RUN_TARGET_CHECKOUT_LABEL;
}
