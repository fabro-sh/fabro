import type { ErrorResponseEntry, RunStatusResponse } from "@qltysh/fabro-api-client";

import {
  ApiError,
  apiData,
  apiResponse,
  requestSignalOptions,
  runsApi,
} from "./api-client";
import type { RunStatus } from "../data/runs";

export type LifecycleAction = "cancel" | "archive" | "unarchive";

export interface LifecycleActionError {
  status: number;
  errors: ErrorResponseEntry[];
}

const CANCELABLE_STATUSES = new Set<RunStatus>([
  "submitted",
  "queued",
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

export async function cancelRun(id: string, request?: Request): Promise<RunStatusResponse> {
  return runLifecycleAction(id, "cancel", request);
}

export async function archiveRun(id: string, request?: Request): Promise<RunStatusResponse> {
  return runLifecycleAction(id, "archive", request);
}

export async function unarchiveRun(id: string, request?: Request): Promise<RunStatusResponse> {
  return runLifecycleAction(id, "unarchive", request);
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

export function canArchive(status: string | null | undefined): boolean {
  return !!status && ARCHIVABLE_STATUSES.has(status as RunStatus);
}

export function canUnarchive(status: string | null | undefined): boolean {
  return status === "archived";
}

export function canDelete(status: string | null | undefined): boolean {
  return status === "archived";
}

export function isTerminalCancelledRun(run: RunStatusResponse): boolean {
  return run.status.kind === "failed" && run.status.reason === "cancelled";
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
        case "archive":
          return "Only terminal runs can be archived.";
        case "unarchive":
          return "Active runs can't be unarchived.";
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
    case "archive":
      return "Couldn't archive the run right now. Try again.";
    case "unarchive":
      return "Couldn't unarchive the run right now. Try again.";
  }
}

async function runLifecycleAction(
  id: string,
  action: LifecycleAction,
  request?: Request,
): Promise<RunStatusResponse> {
  try {
    switch (action) {
      case "cancel":
        return await apiData(() => runsApi.cancelRun(id, requestSignalOptions(request)));
      case "archive":
        return await apiData(() => runsApi.archiveRun(id, requestSignalOptions(request)));
      case "unarchive":
        return await apiData(() => runsApi.unarchiveRun(id, requestSignalOptions(request)));
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
