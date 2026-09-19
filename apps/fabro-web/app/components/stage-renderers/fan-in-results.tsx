import {
  CheckCircleIcon,
  CpuChipIcon,
} from "@heroicons/react/20/solid";

import type { Stage } from "../stage-sidebar";
import { formatTokenCount } from "../../lib/format";
import { Markdown } from "./primitives";
import { StageMetaBar } from "./meta-bar";
import type { ReducerTranscript } from "./helpers";

export function FanInResults({
  stage,
  reducer,
}: {
  stage: Stage;
  /** The reducer's transcript from the projection, or null when none ran. */
  reducer: ReducerTranscript | null;
}) {
  return (
    <div className="space-y-6 pl-3 pr-4 sm:pr-6 lg:pr-8">
      <StageMetaBar stage={stage}>
        {reducer?.model ? (
          <span className="inline-flex items-center gap-1.5 text-xs text-fg-muted">
            <CpuChipIcon className="size-3.5" aria-hidden="true" />
            <span className="font-mono">{reducer.model}</span>
          </span>
        ) : null}
      </StageMetaBar>

      <section className="overflow-hidden rounded-lg bg-gradient-to-br from-mint/10 via-panel to-panel outline-1 -outline-offset-1 outline-line">
        <div className="flex flex-col gap-5 p-6 sm:flex-row sm:items-center sm:gap-8">
          <div className="flex size-14 shrink-0 items-center justify-center rounded-full bg-mint/15 ring-1 ring-mint/30">
            <CheckCircleIcon className="size-7 text-mint" aria-hidden="true" />
          </div>
          <div className="min-w-0 flex-1">
            <div className="text-[10px] font-semibold uppercase tracking-[0.18em] text-mint">
              Joined
            </div>
            <p className="mt-1 text-sm text-fg-3">
              Parallel branches rejoined the workflow.
            </p>
          </div>
        </div>
      </section>

      {reducer && (
        <section className="space-y-4">
          <h3 className="text-xs font-medium uppercase tracking-wider text-fg-muted">
            Reducer transcript
          </h3>

          <article className="rounded-lg bg-panel p-4 outline-1 -outline-offset-1 outline-line">
            <header className="mb-2 flex items-center gap-2 text-[10px] font-medium uppercase tracking-wider">
              <span className="rounded-full bg-amber/15 px-2 py-0.5 text-amber">
                Prompt
              </span>
            </header>
            {reducer.prompt ? (
              <Markdown content={reducer.prompt} />
            ) : (
              <p className="text-sm text-fg-muted">No prompt recorded.</p>
            )}
          </article>

          <article className="rounded-lg bg-panel p-4 outline-1 -outline-offset-1 outline-line">
            <header className="mb-2 flex items-center gap-2 text-[10px] font-medium uppercase tracking-wider">
              <span className="rounded-full bg-teal-500/15 px-2 py-0.5 text-teal-500">
                Response
              </span>
              {(reducer.inputTokens > 0 || reducer.outputTokens > 0) && (
                <span className="ml-auto font-mono normal-case tracking-normal text-fg-muted">
                  {formatTokenCount(reducer.inputTokens)} / {formatTokenCount(reducer.outputTokens)} tokens
                </span>
              )}
            </header>
            {reducer.response ? (
              <Markdown content={reducer.response} />
            ) : (
              <p className="text-sm text-fg-muted">No response recorded.</p>
            )}
          </article>
        </section>
      )}
    </div>
  );
}
