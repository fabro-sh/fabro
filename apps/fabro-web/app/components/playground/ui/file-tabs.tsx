import { useMemo, useState } from "react";

import type { WorkflowDraft } from "../state/draft";
import { renderFabro } from "../files/render-fabro";
import { renderProjectToml, renderWorkflowToml } from "../files/render-toml";
import { renderReadme } from "../files/render-readme";

/** A single file the user can preview before downloading the zip. */
type FileTab = {
  id: string;
  label: string;
  language: string;
  render: (draft: WorkflowDraft) => string;
};

const TABS: FileTab[] = [
  {
    id: "workflow.fabro",
    label: "workflow.fabro",
    language: "dot",
    render: (draft) => renderFabro(draft),
  },
  {
    id: "workflow.toml",
    label: "workflow.toml",
    language: "toml",
    render: (draft) => renderWorkflowToml(draft),
  },
  {
    id: "project.toml",
    label: "project.toml",
    language: "toml",
    render: (draft) => renderProjectToml(draft),
  },
  {
    id: "README.md",
    label: "README.md",
    language: "markdown",
    render: (draft) => renderReadme(draft),
  },
];

/**
 * Tabbed preview of the four files that ship in the downloaded zip.
 *
 * All four are live-rendered from the draft (no debounce — file generation
 * is microseconds at the sizes a playground workflow reaches). The active
 * tab persists in local component state, not the draft, so switching tabs
 * never alters what gets downloaded.
 */
export default function FileTabs({ draft }: { draft: WorkflowDraft }) {
  const [activeId, setActiveId] = useState(TABS[0]!.id);
  const active = TABS.find((t) => t.id === activeId) ?? TABS[0]!;

  const body = useMemo(() => active.render(draft), [active, draft]);

  return (
    <div className="flex h-full min-h-0 flex-col overflow-hidden rounded-md border border-line bg-panel-alt/40">
      <div
        role="tablist"
        aria-label="Generated files"
        className="flex shrink-0 items-center gap-0.5 border-b border-line px-1.5 py-1.5"
      >
        {TABS.map((tab) => {
          const isActive = tab.id === activeId;
          return (
            <button
              type="button"
              key={tab.id}
              role="tab"
              aria-selected={isActive}
              onClick={() => setActiveId(tab.id)}
              className={[
                "rounded px-2.5 py-1 font-mono text-[11.5px] transition-colors",
                isActive
                  ? "bg-teal-500/10 text-teal-200 ring-1 ring-teal-500/30"
                  : "text-fg-muted hover:bg-overlay hover:text-fg-3",
              ].join(" ")}
            >
              {tab.label}
            </button>
          );
        })}
      </div>

      <pre
        role="tabpanel"
        aria-label={active.label}
        data-language={active.language}
        className="m-0 min-h-0 flex-1 overflow-auto p-4 font-mono text-[12px] leading-relaxed text-fg-2"
      >
        {body}
      </pre>
    </div>
  );
}
