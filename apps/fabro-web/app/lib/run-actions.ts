import type { ErrorResponseEntry, RunStatusResponse } from "@qltysh/fabro-api-client";

import { apiFetch } from "../api";
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

export function canCancel(status: string | null | undefined): boolean {
  return !!status && CANCELABLE_STATUSES.has(status as RunStatus);
}

export function canArchive(status: string | null | undefined): boolean {
  return !!status && ARCHIVABLE_STATUSES.has(status as RunStatus);
}

export function canUnarchive(status: string | null | undefined): boolean {
  return status === "archived";
}

export function isTerminalCancelledRun(run: RunStatusResponse): boolean {
  return (run.status === "failed" || run.status === "dead") && run.status_reason === "cancelled";
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
  const response = await apiFetch(`/runs/${id}/${action}`, {
    init: {
      method: "POST",
      ...(request?.signal ? { signal: request.signal } : {}),
    },
  });

  if (!response.ok) {
    throw await parseLifecycleActionError(response);
  }

  return response.json() as Promise<RunStatusResponse>;
}

async function parseLifecycleActionError(response: Response): Promise<LifecycleActionError> {
  let bodyText = "";
  try {
    bodyText = await response.text();
  } catch {
    // Ignore body read failures and fall back to the status only.
  }

  if (!bodyText) {
    return { status: response.status, errors: [] };
  }

  try {
    const body = JSON.parse(bodyText) as { errors?: unknown };
    if (!Array.isArray(body.errors)) {
      return { status: response.status, errors: [] };
    }

    const errors = body.errors.filter(isErrorResponseEntry);
    return { status: response.status, errors };
  } catch {
    return { status: response.status, errors: [] };
  }
}

function isLifecycleActionError(value: unknown): value is LifecycleActionError {
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
