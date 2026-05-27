import type {
  Run,
  RunProjection,
  RunSandbox,
  RunSandboxInstance,
  RunSandboxKind,
  RunSandboxRuntime,
} from "@qltysh/fabro-api-client";

export type SandboxLifecycleKind = RunSandboxKind;

export type MaybeSandbox = Run["sandbox"] | RunProjection["sandbox"] | null | undefined;

export const SANDBOX_LIFECYCLE_DISPLAY: Record<
  SandboxLifecycleKind,
  { label: string; description: string; dot: string; text: string }
> = {
  planned: {
    label:       "Not created",
    description: "The sandbox instance was not created.",
    dot:         "bg-fg-muted",
    text:        "text-fg-muted",
  },
  initializing: {
    label:       "Initializing",
    description: "The sandbox is being created.",
    dot:         "bg-amber",
    text:        "text-amber",
  },
  ready: {
    label:       "Ready",
    description: "The sandbox instance is available.",
    dot:         "bg-teal-500",
    text:        "text-teal-500",
  },
  failed: {
    label:       "Failed",
    description: "Sandbox creation failed.",
    dot:         "bg-coral",
    text:        "text-coral",
  },
};

export function sandboxLifecycleKind(
  sandbox: MaybeSandbox,
): SandboxLifecycleKind | null {
  if (!sandbox) return null;
  const value = sandbox as RunSandbox & {
    provider?: unknown;
    runtime?: unknown;
  };
  if (value.kind) return value.kind as SandboxLifecycleKind;
  return value.runtime ? "ready" : "planned";
}

export function sandboxInstance(
  sandbox: MaybeSandbox,
): RunSandboxInstance | null {
  if (!sandbox) return null;
  const value = sandbox as RunSandbox & {
    provider?: RunSandboxInstance["provider"];
    image?: string | null;
    snapshot?: string | null;
    runtime?: RunSandboxRuntime | null;
  };
  if (value.instance) return value.instance;
  if (value.runtime && value.provider) {
    return {
      provider: value.provider,
      image:    value.image ?? null,
      snapshot: value.snapshot ?? null,
      runtime:  value.runtime,
    };
  }
  return null;
}

export function sandboxRuntime(
  sandbox: MaybeSandbox,
): RunSandboxRuntime | null {
  return sandboxInstance(sandbox)?.runtime ?? null;
}

export function sandboxTabVisible(sandbox: MaybeSandbox): boolean {
  const kind = sandboxLifecycleKind(sandbox);
  return kind === "initializing" || kind === "ready" || kind === "failed";
}

export function sandboxIsReady(sandbox: MaybeSandbox): boolean {
  return sandboxLifecycleKind(sandbox) === "ready" && sandboxInstance(sandbox) != null;
}
