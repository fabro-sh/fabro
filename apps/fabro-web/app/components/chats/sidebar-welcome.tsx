import { ThreadPrimitive } from "@assistant-ui/react";
import {
  BoltIcon,
  ExclamationTriangleIcon,
  LightBulbIcon,
  MapIcon,
} from "@heroicons/react/16/solid";

/**
 * Example prompts shown on the empty Ask-Fabro thread. Each `prompt` is sent
 * verbatim as the first message; `heading`/`description` are display-only.
 */
const EXAMPLE_PROMPTS = [
  {
    Icon: ExclamationTriangleIcon,
    heading: "Surface errors",
    description: "Find errors, warnings, and failed steps in this run.",
    prompt:
      "Analyze this workflow run for any errors, exceptions, failed operations, or unexpected behavior. Include details about what went wrong, when it occurred, and potential causes or fixes.",
  },
  {
    Icon: BoltIcon,
    heading: "Analyze performance",
    description: "Spot the slowest stages and where time was spent.",
    prompt:
      "Analyze the performance of this workflow run, including tool execution times, API call latencies, timeouts, and any bottlenecks. Compare timing across similar operations and identify optimization opportunities.",
  },
  {
    Icon: MapIcon,
    heading: "Review key decisions",
    description: "Recap the important choices the agent made.",
    prompt:
      "Trace the workflow run flow, including key decision points, branching logic, tool selection reasoning, and how the assistant responded to different inputs or contexts.",
  },
  {
    Icon: LightBulbIcon,
    heading: "Suggest improvements",
    description: "Ideas to make this workflow faster and more reliable.",
    prompt:
      "Provide recommendations for improving this workflow, including better graph design, prompting strategies, more efficient tool usage, error handling improvements, and ways to optimize the overall user experience.",
  },
];

/**
 * Empty-state for the Ask-Fabro sidebar. Wrapped in `ThreadPrimitive.Empty` so
 * it renders only before the first message; clicking an example sends its
 * prompt, which starts the session.
 */
export default function SidebarWelcome() {
  return (
    <ThreadPrimitive.Empty>
      <div className="flex flex-col gap-6 px-4 py-8">
        <h2 className="text-base font-semibold text-fg">How can I help?</h2>
        <ul className="flex flex-col gap-3">
          {EXAMPLE_PROMPTS.map((example) => (
            <li key={example.heading}>
              <ThreadPrimitive.Suggestion asChild prompt={example.prompt} send>
                <button
                  type="button"
                  className="flex w-full items-start gap-3 rounded-xl bg-panel-alt/60 px-4 py-3.5 text-left ring-1 ring-line transition-colors hover:bg-panel-alt hover:ring-line-strong focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
                >
                  <example.Icon
                    aria-hidden="true"
                    className="size-4 h-lh shrink-0 fill-fg-3"
                  />
                  <div className="flex flex-col gap-1">
                    <p className="text-sm font-medium text-fg">
                      {example.heading}
                    </p>
                    <p className="text-xs text-fg-3">{example.description}</p>
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
