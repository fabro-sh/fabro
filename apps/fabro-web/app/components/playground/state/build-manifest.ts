/**
 * Build a `RunManifest` from the current `WorkflowDraft`.
 *
 * Inline-everything style: the manifest carries the full DOT and
 * `workflow.toml` source in `workflows[key].{source, config}`, so the
 * server doesn't need a temp dir or git commit. See the runs-API
 * notes in `playground_build_plan.md` for the schema details.
 */
import { renderFabro } from "../files/render-fabro";
import { renderWorkflowToml } from "../files/render-toml";
import {
  DEFAULT_NAME,
  FALLBACK_DOWNLOAD_NAME,
  isValidWorkflowName,
  type WorkflowDraft,
} from "./draft";

/**
 * Minimal subset of `RunManifest` the playground needs to send. The
 * generated `RunManifest` type from `@qltysh/fabro-api-client` accepts
 * the same shape; we keep this lightweight so the playground subtree
 * doesn't pick up an extra dep.
 */
export interface PlaygroundRunManifest {
  version: 1;
  cwd: string;
  title?: string;
  target: {
    identifier: string;
    path: string;
  };
  workflows: {
    [path: string]: {
      source: string;
      config?: {
        path: string;
        source: string;
      };
    };
  };
}

/**
 * Resolve the workflow identifier used in the manifest paths. Mirrors
 * the download-zip filename logic so a user who downloaded and a user
 * who hit "Run for real" end up with the same artifact name.
 */
export function resolveWorkflowName(draft: WorkflowDraft): string {
  if (draft.name && draft.name !== DEFAULT_NAME && isValidWorkflowName(draft.name)) {
    return draft.name;
  }
  return FALLBACK_DOWNLOAD_NAME.replace(/-/g, "_");
}

export function buildRunManifest(draft: WorkflowDraft): PlaygroundRunManifest {
  const name = resolveWorkflowName(draft);
  const workflowPath = `.fabro/workflows/${name}/workflow.fabro`;
  return {
    version: 1,
    cwd:     `/tmp/fabro-playground-${name}`,
    title:   draft.goal && draft.goal.length > 0 ? draft.goal : `Playground: ${name}`,
    target:  {
      identifier: name,
      path:       workflowPath,
    },
    workflows: {
      [workflowPath]: {
        source: renderFabro(draft),
        config: {
          path:   "workflow.toml",
          source: renderWorkflowToml(draft),
        },
      },
    },
  };
}
