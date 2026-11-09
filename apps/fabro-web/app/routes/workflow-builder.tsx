import { Suspense, lazy, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router";

import { ErrorState, LoadingState } from "../components/state";
import { SECONDARY_BUTTON_CLASS } from "../components/ui";
import { importChunk } from "../lib/import-chunk";
import { newDraft } from "../lib/workflow-builder/model";
import { loadDraft } from "../lib/workflow-builder/storage";
import type { WorkflowDraft } from "../lib/workflow-builder/types";

// hideHeader/fullHeight/wide let the canvas fill everything below the top nav.
export const handle = { hideHeader: true, fullHeight: true, wide: true };

// React Flow is only needed here, so it loads with the builder rather than the app.
const WorkflowBuilder = lazy(() =>
  importChunk(() => import("../components/workflow-builder/builder")),
);

const NEW_DRAFT_PARAM = "new";

type Opened =
  | { wanted: string; status: "ready"; draft: WorkflowDraft; persisted: boolean }
  | { wanted: string; status: "missing" }
  | { wanted: string; status: "failed"; message: string };

function openDraft(id: string, wanted: string): Opened {
  if (id === NEW_DRAFT_PARAM) return { wanted, status: "ready", draft: newDraft(), persisted: false };
  try {
    const draft = loadDraft(id);
    return draft ? { wanted, status: "ready", draft, persisted: true } : { wanted, status: "missing" };
  } catch (error) {
    return { wanted, status: "failed", message: error instanceof Error ? error.message : String(error) };
  }
}

/**
 * `/workflows/new` and `/workflows/:id` are one route. A new draft stays in memory
 * until its first edit, so Create leaves nothing behind if abandoned. Then the URL
 * swaps to the saved ID in place, and because the route doesn't change, the editor
 * stays mounted with its focus and undo history.
 */
export default function WorkflowBuilderRoute({ params }: { params: { id: string } }) {
  const location = useLocation();
  const navigate = useNavigate();
  // Each visit to Create is a fresh draft, even from a draft that is already new.
  const wanted = params.id === NEW_DRAFT_PARAM ? `new:${location.key}` : `saved:${params.id}`;
  const [opened, setOpened] = useState<Opened>(() => openDraft(params.id, wanted));

  let current = opened;
  const stillThisDraft = opened.status === "ready" && opened.draft.id === params.id;
  if (opened.wanted !== wanted && !stillThisDraft) {
    current = openDraft(params.id, wanted);
    setOpened(current);
  }

  return (
    <div className="wfb-route wfb-theme">
      {current.status === "ready" ? (
        <Suspense
          fallback={
            <div className="p-8">
              <LoadingState label="Loading the builder…" />
            </div>
          }
        >
          <WorkflowBuilder
            key={current.draft.id}
            initialDraft={current.draft}
            persisted={current.persisted}
            onFirstSave={(id) => navigate(`/workflows/${id}${location.search}`, { replace: true })}
          />
        </Suspense>
      ) : current.status === "missing" ? (
        <div className="p-8">
          <ErrorState
            title="This draft isn't in this browser"
            description="Drafts are saved in the browser where they were made. It may have been deleted, or it lives in another browser."
          />
          <div className="mt-4 flex justify-center">
            <Link to="/workflows" className={SECONDARY_BUTTON_CLASS}>
              Back to workflows
            </Link>
          </div>
        </div>
      ) : (
        <div className="p-8">
          <ErrorState title="This draft couldn't be opened" description={current.message} />
          <div className="mt-4 flex justify-center">
            <Link to="/workflows" className={SECONDARY_BUTTON_CLASS}>
              Back to workflows
            </Link>
          </div>
        </div>
      )}
    </div>
  );
}
