import type { BatchRunLifecycleSummary } from "@qltysh/fabro-api-client";

import { plural } from "../../lib/plural";

export type BatchLifecycleLabel = "Archive" | "Unarchive" | "Delete" | "Approve";

export interface BatchLifecycleToast {
  message: string;
  tone?: "error";
}

export function summarizeBatchLifecycleAction(
  label: BatchLifecycleLabel,
  summary: BatchRunLifecycleSummary,
): BatchLifecycleToast {
  const { requested, succeeded, failed } = summary;
  if (failed === 0) {
    return { message: `${label}d ${succeeded} ${plural(succeeded, "run", "runs")}.` };
  }
  if (succeeded === 0) {
    return {
      message: `Couldn't ${label.toLowerCase()} ${requested} ${plural(requested, "run", "runs")}. Try again.`,
      tone:    "error",
    };
  }
  return {
    message: `${label}d ${succeeded} of ${requested} ${plural(requested, "run", "runs")}. ${failed} failed.`,
    tone:    "error",
  };
}
