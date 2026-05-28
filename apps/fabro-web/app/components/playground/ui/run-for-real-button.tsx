import { useState } from "react";
import { RocketLaunchIcon } from "@heroicons/react/24/outline";

import type { WorkflowDraft } from "../state/draft";
import { isWelcomeState } from "../state/draft";
import RunForRealModal from "./run-for-real-modal";

export interface RealRunRedirect {
  href: string;
  /** Button label override. Defaults to "Run for real". */
  label?: string;
}

/**
 * "Run for real" toolbar button. Two modes:
 *
 * - **Default** (fabro-web): the button opens a confirmation modal that
 *   POSTs the workflow to `/api/v1/runs` and redirects to the resulting
 *   run page.
 * - **Redirect** (custom embed): when `redirect` is supplied, the button
 *   renders as a plain anchor pointing at the configured href. Use this
 *   in embed contexts that have no backend to launch against, to send
 *   visitors to a CTA URL (e.g. `/download`) instead.
 *
 * Disabled in the welcome state for the default mode — running an empty
 * workflow is pointless. The redirect mode stays enabled because the
 * destination is informational, not a real run.
 */
export default function RunForRealButton({
  draft,
  redirect,
}: {
  draft: WorkflowDraft;
  redirect?: RealRunRedirect;
}) {
  const [isOpen, setIsOpen] = useState(false);

  const classes =
    "inline-flex items-center gap-1.5 rounded-md bg-fuchsia-500/10 px-3 py-1.5 text-sm font-medium text-fuchsia-200 ring-1 ring-fuchsia-500/30 transition-colors hover:bg-fuchsia-500/20 hover:text-fuchsia-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-fuchsia-500 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-fuchsia-500/10 disabled:hover:text-fuchsia-200";

  if (redirect) {
    return (
      <a href={redirect.href} className={classes}>
        <RocketLaunchIcon className="size-4" />
        {redirect.label ?? "Run for real"}
      </a>
    );
  }

  const disabled = isWelcomeState(draft);
  return (
    <>
      <button
        type="button"
        disabled={disabled}
        title={disabled ? "Add at least one node first" : undefined}
        onClick={() => setIsOpen(true)}
        className={classes}
      >
        <RocketLaunchIcon className="size-4" />
        Run for real
      </button>
      {isOpen && (
        <RunForRealModal draft={draft} onClose={() => setIsOpen(false)} />
      )}
    </>
  );
}
