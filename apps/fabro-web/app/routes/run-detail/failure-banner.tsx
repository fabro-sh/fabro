import { ChevronDownIcon, ExclamationTriangleIcon } from "@heroicons/react/20/solid";

import type { RunFailureView } from "./failure";

export function RunFailureBanner({ failure }: { failure: RunFailureView }) {
  return (
    <section aria-label="Why this run failed" className="mb-6">
      <details className="group rounded-lg bg-panel-alt outline-1 -outline-offset-1 outline-line">
        <summary className="flex cursor-pointer list-none items-start gap-3 rounded-lg px-4 py-3.5 transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-current [&::-webkit-details-marker]:hidden">
          <ExclamationTriangleIcon
            className="mt-0.5 size-4 shrink-0 text-coral"
            aria-hidden="true"
          />
          <span className="min-w-0 flex-1">
            <span className="block text-sm/5 font-medium text-fg">
              {failure.stage ? (
                <>
                  Stopped at <span className="font-mono text-[0.8125rem]">{failure.stage}</span>
                  <span className="mx-2 text-fg-muted" aria-hidden="true">
                    ·
                  </span>
                </>
              ) : null}
              {failure.headline}
            </span>
            <span className="mt-1 block text-sm/5 text-fg-3">{failure.nextStep}</span>
          </span>
          <span className="inline-flex shrink-0 items-center gap-1 pt-0.5 text-xs font-medium text-fg-muted transition-colors group-hover:text-fg-3">
            <span className="group-open:hidden">Details</span>
            <span className="hidden group-open:inline">Hide</span>
            <ChevronDownIcon
              className="size-3.5 shrink-0 transition-transform group-open:-rotate-180"
              aria-hidden="true"
            />
          </span>
        </summary>
        <dl className="space-y-3 px-4 pt-0.5 pb-4 pl-11">
          <div>
            <dt className="mb-1 font-mono text-[0.6875rem] tracking-wide text-fg-muted uppercase">
              {failure.provider ? "Provider message" : "Message"}
            </dt>
            <dd className="max-h-48 overflow-auto rounded-md bg-overlay px-3 py-2 font-mono text-xs/5 wrap-break-word whitespace-pre-wrap text-fg-3">
              {failure.message}
            </dd>
          </div>
          {failure.output ? (
            <div>
              <dt className="mb-1 font-mono text-[0.6875rem] tracking-wide text-fg-muted uppercase">
                Output
              </dt>
              <dd>
                <pre className="max-h-48 overflow-auto rounded-md bg-overlay px-3 py-2 font-mono text-xs/5 text-fg-3">
                  {failure.output}
                </pre>
              </dd>
            </div>
          ) : null}
        </dl>
      </details>
    </section>
  );
}
