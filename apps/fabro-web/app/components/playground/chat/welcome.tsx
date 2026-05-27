import { ThreadPrimitive } from "@assistant-ui/react";
import {
  ArrowPathIcon,
  ClipboardDocumentCheckIcon,
  DocumentTextIcon,
  WrenchScrewdriverIcon,
} from "@heroicons/react/16/solid";

/**
 * Suggestion chips on the empty playground thread. Per the spec, clicking
 * a chip *fills the composer* rather than auto-sending — the user sees
 * their prompt go in and can tweak before hitting send.
 *
 * `ThreadPrimitive.Suggestion` without the `send` prop does exactly that.
 */
const SUGGESTIONS = [
  {
    Icon:        ClipboardDocumentCheckIcon,
    heading:     "Daily standup summary",
    description: "Summarize yesterday's commits into a standup post.",
    prompt:
      "Build a workflow that pulls yesterday's git commits and summarises them into a Slack-ready daily standup post.",
  },
  {
    Icon:        WrenchScrewdriverIcon,
    heading:     "Lint, test, and open a PR",
    description: "Test the diff and ship it as a draft PR if it passes.",
    prompt:
      "Build a workflow that lints, runs the tests, and opens a draft pull request only if everything passes.",
  },
  {
    Icon:        DocumentTextIcon,
    heading:     "Release notes",
    description: "Generate notes from git log between two tags.",
    prompt:
      "Build a workflow that takes two git tags and produces release notes from the commits between them.",
  },
  {
    Icon:        ArrowPathIcon,
    heading:     "Triage a GitHub issue",
    description: "Label, summarise, and assign new issues.",
    prompt:
      "Build a workflow that takes a fresh GitHub issue and triages it: choose labels, summarise the report, and assign an owner.",
  },
];

export default function PlaygroundWelcome() {
  return (
    <ThreadPrimitive.Empty>
      <div className="flex flex-col gap-6 px-4 py-8">
        <div>
          <h2 className="text-base font-semibold text-fg">
            What workflow are you trying to build?
          </h2>
          <p className="mt-1 text-xs text-fg-3">
            Describe it and I&apos;ll sketch the graph on the canvas. Pick one
            below to start, or write your own.
          </p>
        </div>
        <ul className="flex flex-col gap-3">
          {SUGGESTIONS.map((s) => (
            <li key={s.heading}>
              <ThreadPrimitive.Suggestion asChild prompt={s.prompt}>
                <button
                  type="button"
                  className="flex w-full items-start gap-3 rounded-xl bg-panel-alt/60 px-4 py-3.5 text-left ring-1 ring-line transition-colors hover:bg-panel-alt hover:ring-line-strong focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
                >
                  <s.Icon
                    aria-hidden="true"
                    className="size-4 h-lh shrink-0 fill-teal-300"
                  />
                  <div className="flex flex-col gap-1">
                    <p className="text-sm font-medium text-fg">{s.heading}</p>
                    <p className="text-xs text-fg-3">{s.description}</p>
                  </div>
                </button>
              </ThreadPrimitive.Suggestion>
            </li>
          ))}
        </ul>
      </div>
    </ThreadPrimitive.Empty>
  );
}
