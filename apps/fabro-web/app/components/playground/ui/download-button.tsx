import { ArrowDownTrayIcon } from "@heroicons/react/24/outline";

import type { WorkflowDraft } from "../state/draft";
import {
  buildDownloadBundle,
  triggerDownload,
} from "../files/download";

/**
 * Bundles the draft into the `.fabro.zip` layout and kicks a browser
 * download. The pure parts of that flow live in `files/download.ts`; this
 * component only owns the click handler and the visual treatment.
 *
 * Enabled even in the welcome state — the artifact is still a runnable,
 * minimal workflow worth taking away.
 */
export default function DownloadButton({ draft }: { draft: WorkflowDraft }) {
  return (
    <button
      type="button"
      onClick={() => triggerDownload(buildDownloadBundle(draft))}
      className="inline-flex items-center gap-1.5 rounded-md bg-teal-500/10 px-3 py-1.5 text-sm font-medium text-teal-200 ring-1 ring-teal-500/30 transition-colors hover:bg-teal-500/20 hover:text-teal-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
    >
      <ArrowDownTrayIcon className="size-4" />
      Download .fabro.zip
    </button>
  );
}
