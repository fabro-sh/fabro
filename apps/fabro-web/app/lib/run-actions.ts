import type {
  BatchRunLifecycleRequest,
  BatchRunLifecycleResponse,
  ErrorResponseEntry,
  Run,
} from "@qltysh/fabro-api-client";

import {
  ApiError,
  apiData,
  apiResponse,
  requestSignalOptions,
  runsApi,
} from "./api-client";
import type { RunStatus } from "../data/runs";

export type LifecycleAction =
  | "cancel"
  | "approve"
  | "deny"
  | "archive"
  | "unarchive"
  | "retry";

export interface LifecycleActionError {
  status: number;
  errors: ErrorResponseEntry[];
}

const CANCELABLE_STATUSES = new Set<RunStatus>([
  "submitted",
  "pending",
  "runnable",
  "starting",
  "running",
  "paused",
  "blocked",
]);

const ARCHIVABLE_STATUSES = new Set<RunStatus>([
  "succeeded",
  "failed",
  "dead",
]);

export async function cancelRun(id: string, request?: Request): Promise<Run> {
  return runLifecycleAction(id, "cancel", request);
}

export async function approveRun(id: string, request?: Request): Promise<Run> {
  return runLifecycleAction(id, "approve", request);
}

export async function denyRun(id: string, request?: Request): Promise<Run> {
  return runLifecycleAction(id, "deny", request);
}

export async function archiveRun(id: string, request?: Request): Promise<Run> {
  return runLifecycleAction(id, "archive", request);
}

export async function unarchiveRun(id: string, request?: Request): Promise<Run> {
  return runLifecycleAction(id, "unarchive", request);
}

export async function archiveRuns(
  runIds: string[],
  request?: Request,
): Promise<BatchRunLifecycleResponse> {
  return batchRunLifecycleAction(runIds, "archive", request);
}

export async function unarchiveRuns(
  runIds: string[],
  request?: Request,
): Promise<BatchRunLifecycleResponse> {
  return batchRunLifecycleAction(runIds, "unarchive", request);
}

export async function retryRun(id: string, request?: Request): Promise<Run> {
  return runLifecycleAction(id, "retry", request);
}

export async function deleteRun(id: string, request?: Request): Promise<void> {
  try {
    await apiResponse(() => runsApi.deleteRun(id, undefined, requestSignalOptions(request)));
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return;
    throw lifecycleActionErrorFromError(error);
  }
}

export function canCancel(status: string | null | undefined): boolean {
  return !!status && CANCELABLE_STATUSES.has(status as RunStatus);
}

export function canApprove(run: Run | null | undefined): boolean {
  return run?.lifecycle.status.kind === "pending" && run.lifecycle.approval?.state === "pending";
}

export function canArchive(status: string | null | undefined): boolean {
  return !!status && ARCHIVABLE_STATUSES.has(status as RunStatus);
}

export function canUnarchive(status: string | null | undefined): boolean {
  return status === "archived";
}

export function canRetry(run: Pick<Run, "lifecycle"> | null | undefined): boolean {
  if (!run || run.lifecycle.archived) return false;
  const status = run.lifecycle.status;
  if (status.kind === "dead") return true;
  return status.kind === "failed" && status.reason !== "cancelled";
}

export function canDelete(status: string | null | undefined): boolean {
  return status === "archived";
}

export function isTerminalCancelledRun(run: Run): boolean {
  const status = run.lifecycle.status;
  return status.kind === "failed" && status.reason === "cancelled";
}

export function deleteErrorMessage(error: unknown): string {
  if (isLifecycleActionError(error)) {
    if (error.status === 409) {
      return "Active runs can't be deleted.";
    }
    const detail = error.errors[0]?.detail?.trim();
    if (detail) return detail;
  }
  return "Couldn't delete the run right now. Try again.";
}

export function mapError(error: unknown, action: LifecycleAction): string {
  if (isLifecycleActionError(error)) {
    if (error.status === 404) {
      return "This run no longer exists.";
    }
    if (error.status === 409) {
      switch (action) {
        case "cancel":
          return "This run can no longer be cancelled.";
        case "approve":
        case "deny":
          return "This run is no longer pending approval.";
        case "archive":
          return "Only terminal runs can be archived.";
        case "unarchive":
          return "Active runs can't be unarchived.";
        case "retry":
          return "This run can no longer be retried.";
      }
    }

    const detail = error.errors[0]?.detail?.trim();
    if (detail) {
      return detail;
    }
  }

  switch (action) {
    case "cancel":
      return "Couldn't cancel the run right now. Try again.";
    case "approve":
      return "Couldn't approve the run right now. Try again.";
    case "deny":
      return "Couldn't deny the run right now. Try again.";
    case "archive":
      return "Couldn't archive the run right now. Try again.";
    case "unarchive":
      return "Couldn't unarchive the run right now. Try again.";
    case "retry":
      return "Couldn't retry the run right now. Try again.";
  }
}

async function runLifecycleAction(
  id: string,
  action: LifecycleAction,
  request?: Request,
): Promise<Run> {
  try {
    switch (action) {
      case "cancel":
        return await apiData(() => runsApi.cancelRun(id, requestSignalOptions(request)));
      case "approve":
        return await apiData(() => runsApi.approveRun(id, requestSignalOptions(request)));
      case "deny":
        return await apiData(() => runsApi.denyRun(id, undefined, requestSignalOptions(request)));
      case "archive":
        return await apiData(() => runsApi.archiveRun(id, requestSignalOptions(request)));
      case "unarchive":
        return await apiData(() => runsApi.unarchiveRun(id, requestSignalOptions(request)));
      case "retry":
        return await apiData(() => runsApi.retryRun(id, requestSignalOptions(request)));
    }
  } catch (error) {
    throw lifecycleActionErrorFromError(error);
  }
}

async function batchRunLifecycleAction(
  runIds: string[],
  action: "archive" | "unarchive",
  request?: Request,
): Promise<BatchRunLifecycleResponse> {
  try {
    // openapi-generator's TypeScript client represents `uniqueItems` arrays as
    // Set<T>, but the HTTP wire contract is still a JSON array. Keep an array
    // here so Axios serializes the request body correctly.
    const body = { run_ids: runIds } as unknown as BatchRunLifecycleRequest;
    switch (action) {
      case "archive":
        return await apiData(() => runsApi.batchArchiveRuns(body, requestSignalOptions(request)));
      case "unarchive":
        return await apiData(() => runsApi.batchUnarchiveRuns(body, requestSignalOptions(request)));
    }
  } catch (error) {
    throw lifecycleActionErrorFromError(error);
  }
}

function lifecycleActionErrorFromError(error: unknown): LifecycleActionError {
  if (!(error instanceof ApiError)) throw error;
  return {
    status: error.status,
    errors: parseLifecycleErrors(error.body),
  };
}

function parseLifecycleErrors(body: unknown): ErrorResponseEntry[] {
  if (!body || typeof body !== "object") return [];
  const errors = (body as { errors?: unknown }).errors;
  if (!Array.isArray(errors)) return [];
  return errors.filter(isErrorResponseEntry);
}

export function isLifecycleActionError(value: unknown): value is LifecycleActionError {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return typeof record.status === "number" && Array.isArray(record.errors);
}

function isErrorResponseEntry(value: unknown): value is ErrorResponseEntry {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.status === "string"
    && typeof record.title === "string"
    && typeof record.detail === "string"
  );
}
