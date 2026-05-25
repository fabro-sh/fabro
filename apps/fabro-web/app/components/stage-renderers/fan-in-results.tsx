import { useMemo } from "react";
import {
  CpuChipIcon,
  SparklesIcon,
  TrophyIcon,
} from "@heroicons/react/20/solid";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import type { Stage } from "../stage-sidebar";
import { formatTokenCount } from "../../lib/format";
import { getString } from "../../lib/unknown";
import { Markdown } from "./primitives";
import { prettyJson } from "./pretty-json";
import { StageMetaBar } from "./meta-bar";
import { parseFanInOutcome } from "./helpers";

interface ReducerTurn {
  prompt: string;
  response: string;
  model: string | null;
  inputTokens: number;
  outputTokens: number;
}

function extractReducerTurn(events: EventEnvelope[]): ReducerTurn | null {
  let prompt = "";
  let response = "";
  let model: string | null = null;
  let inputTokens = 0;
  let outputTokens = 0;
  let hasReducer = false;

  for (const event of events) {
    const props = event.properties ?? {};
    if (event.event === "stage.prompt" && getString(props, "mode") === "fan_in") {
      prompt = getString(props, "text") ?? prompt;
      hasReducer = true;
    } else if (event.event === "prompt.completed") {
      response = getString(props, "response") ?? response;
      model = getString(props, "model") ?? model;
      const billing = props.billing as Record<string, unknown> | undefined;
      if (billing) {
        const it = billing.input_tokens;
        const ot = billing.output_tokens;
        if (typeof it === "number") inputTokens = it;
        if (typeof ot === "number") outputTokens = ot;
      }
      hasReducer = true;
    }
  }

  return hasReducer ? { prompt, response, model, inputTokens, outputTokens } : null;
}

/**
 * The fan-in `stage.prompt.text` is built by the handler as
 * "<prompt>\n\n<json>". Split it for nicer display so the JSON candidate set
 * lands in a code block rather than fighting markdown rendering.
 */
function splitPromptAndCandidates(text: string): { prompt: string; candidatesJson: string } {
  const trimmed = text.trim();
  // Find the start of the JSON envelope. The handler always uses array results
  // so look for the first opening bracket that begins a balanced array/object.
  const idx = (() => {
    for (let i = 0; i < trimmed.length; i += 1) {
      const ch = trimmed[i];
      if (ch === "[" || ch === "{") return i;
    }
    return -1;
  })();
  if (idx < 0) return { prompt: trimmed, candidatesJson: "" };
  const promptPart = trimmed.slice(0, idx).trim();
  const jsonPart = trimmed.slice(idx).trim();
  const pretty = prettyJson(jsonPart);
  return {
    prompt: promptPart,
    candidatesJson: pretty.isJson ? pretty.text : jsonPart,
  };
}

export function FanInResults({
  stage,
  events,
  notes,
}: {
  stage: Stage;
  events: EventEnvelope[];
  notes: string | null;
}) {
  const outcome = useMemo(() => parseFanInOutcome(events, notes), [events, notes]);
  const reducer = useMemo(() => extractReducerTurn(events), [events]);
  const promptParts = useMemo(
    () => (reducer ? splitPromptAndCandidates(reducer.prompt) : null),
    [reducer],
  );

  return (
    <div className="space-y-6 pl-3 pr-4 sm:pr-6 lg:pr-8">
      <StageMetaBar stage={stage}>
        {outcome.reducerModel ? (
          <span className="inline-flex items-center gap-1.5 text-xs text-fg-muted">
            <CpuChipIcon className="size-3.5" aria-hidden="true" />
            <span className="font-mono">{outcome.reducerModel}</span>
          </span>
        ) : null}
      </StageMetaBar>

      <section className="overflow-hidden rounded-lg bg-gradient-to-br from-amber/10 via-panel to-panel outline-1 -outline-offset-1 outline-line">
        <div className="flex flex-col gap-5 p-6 sm:flex-row sm:items-center sm:gap-8">
          <div className="flex size-14 shrink-0 items-center justify-center rounded-full bg-amber/15 ring-1 ring-amber/30">
            <TrophyIcon className="size-7 text-amber" aria-hidden="true" />
          </div>
          <div className="min-w-0 flex-1">
            <div className="text-[10px] font-semibold uppercase tracking-[0.18em] text-amber">
              Selected branch
            </div>
            {outcome.selectedId ? (
              <p className="mt-1 truncate font-mono text-2xl text-fg">
                {outcome.selectedId}
              </p>
            ) : (
              <p className="mt-1 text-sm text-fg-muted">
                Awaiting fan-in completion
              </p>
            )}
            <p className="mt-2 inline-flex items-center gap-1.5 text-xs text-fg-muted">
              {outcome.hasReducerTranscript ? (
                <>
                  <SparklesIcon className="size-3.5" aria-hidden="true" />
                  Selected by LLM reducer
                </>
              ) : (
                <>Selected by heuristic (status &middot; score &middot; id)</>
              )}
            </p>
          </div>
        </div>
      </section>

      {reducer && promptParts && (
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
            {promptParts.prompt && (
              <Markdown content={promptParts.prompt} />
            )}
            {promptParts.candidatesJson && (
              <details className="mt-3 group">
                <summary className="cursor-pointer text-xs text-fg-muted hover:text-fg-2">
                  Candidates JSON
                </summary>
                <pre className="mt-2 max-h-72 overflow-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
                  {promptParts.candidatesJson}
                </pre>
              </details>
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
