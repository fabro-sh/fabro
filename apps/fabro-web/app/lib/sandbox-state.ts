import type { SandboxState } from "@qltysh/fabro-api-client";

export interface SandboxStateDisplay {
  /** Short human-readable label, e.g. "Running". */
  label: string;
  /** One-sentence explanation shown on hover. */
  description: string;
  /** Tailwind background class for the status dot. */
  dot: string;
  /** Tailwind text color class matching the dot. */
  text: string;
}

/**
 * Display metadata for every normalized sandbox lifecycle state. Shared by the
 * run overview summary panel and the dedicated sandbox page so the dot color,
 * label, and hover copy stay consistent.
 */
export const SANDBOX_STATE_DISPLAY: Record<SandboxState, SandboxStateDisplay> = {
  unknown: {
    label: "Unknown",
    description: "The sandbox state could not be determined.",
    dot: "bg-fg-muted",
    text: "text-fg-muted",
  },
  provisioning: {
    label: "Provisioning",
    description: "The sandbox is being provisioned.",
    dot: "bg-amber",
    text: "text-amber",
  },
  starting: {
    label: "Starting",
    description: "The sandbox is starting up.",
    dot: "bg-amber",
    text: "text-amber",
  },
  running: {
    label: "Running",
    description: "The sandbox is running.",
    dot: "bg-teal-500",
    text: "text-teal-500",
  },
  stopping: {
    label: "Stopping",
    description: "The sandbox is shutting down.",
    dot: "bg-amber",
    text: "text-amber",
  },
  stopped: {
    label: "Stopped",
    description: "The sandbox is stopped.",
    dot: "bg-fg-muted",
    text: "text-fg-muted",
  },
  paused: {
    label: "Paused",
    description: "The sandbox is paused.",
    dot: "bg-amber",
    text: "text-amber",
  },
  deleting: {
    label: "Deleting",
    description: "The sandbox is being deleted.",
    dot: "bg-amber",
    text: "text-amber",
  },
  deleted: {
    label: "Deleted",
    description: "The sandbox has been deleted.",
    dot: "bg-coral",
    text: "text-coral",
  },
  archived: {
    label: "Archived",
    description: "The sandbox has been archived.",
    dot: "bg-fg-muted",
    text: "text-fg-muted",
  },
  restoring: {
    label: "Restoring",
    description: "The sandbox is being restored.",
    dot: "bg-amber",
    text: "text-amber",
  },
  resizing: {
    label: "Resizing",
    description: "The sandbox resources are being resized.",
    dot: "bg-amber",
    text: "text-amber",
  },
  error: {
    label: "Error",
    description: "The sandbox encountered an error.",
    dot: "bg-coral",
    text: "text-coral",
  },
};
