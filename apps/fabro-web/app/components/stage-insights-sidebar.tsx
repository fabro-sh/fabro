import { type ReactNode, useCallback, useState } from "react";
import {
  ChevronDoubleLeftIcon,
  ChevronDoubleRightIcon,
  ChevronRightIcon,
} from "@heroicons/react/20/solid";
import {
  ArrowPathIcon,
  CheckCircleIcon,
  XCircleIcon,
} from "@heroicons/react/24/solid";
import {
  CheckBadgeIcon,
  CommandLineIcon,
  ListBulletIcon,
  PuzzlePieceIcon,
  ServerStackIcon,
  Squares2X2Icon,
  WrenchScrewdriverIcon,
} from "@heroicons/react/24/outline";
import {
  AgentSkillActivationSource,
  StageContextWindowCategory,
  StageContextWindowStaleness,
  TodoStatus,
} from "@qltysh/fabro-api-client";
import type {
  ActivatedSkill,
  AgentSkillSummary,
  AgentToolSummary,
  McpServerProjection,
  StageContextWindow,
  StageContextWindowBreakdownItem,
  StageProjection,
  TodoListProjection,
  TodoProjection,
} from "@qltysh/fabro-api-client";
import { formatTokenCount } from "../lib/format";

const COLLAPSED_STORAGE_KEY = "fabro:stage-insights-sidebar-collapsed";
const SECTION_STORAGE_PREFIX = "fabro:stage-insights-section:";

type SectionKey = "todos" | "context" | "tools" | "skills" | "mcps";

const SECTIONS_DEFAULT_OPEN: Record<SectionKey, boolean> = {
  todos:   true,
  context: false,
  tools:   false,
  skills:  false,
  mcps:    false,
};

export interface StageInsightsSidebarProps {
  /** Full stage projection (undefined while loading or for non-agent stages). */
  stage: StageProjection | undefined;
  /** Snapshot from `useRunStageContextWindow`. Null when unavailable. */
  contextWindow: StageContextWindow | null | undefined;
}

export function StageInsightsSidebar({ stage, contextWindow }: StageInsightsSidebarProps) {
  const [collapsed, setCollapsed] = useState(loadStoredCollapsed);
  const toggleCollapsed = useCallback(() => {
    setCollapsed((prev) => {
      const next = !prev;
      persistCollapsed(next);
      return next;
    });
  }, []);

  const todos = stage?.todos ?? null;
  const skills = stage?.skills ?? { activated: [], available: [] };
  const agentTools = stage?.agent_tools ?? [];
  const mcpServers = stage?.mcp_servers ?? [];

  const todoStats = countTodoStats(todos);
  const activatedSkillNames = new Set(skills.activated.map((s) => s.name));
  const invokedToolCount = agentTools.filter((tool) => tool.invoked).length;

  return (
    <aside
      className={`${collapsed ? "w-12" : "w-60"} shrink-0 transition-[width] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)]`}
      aria-label="Agent stage details"
    >
      <div className="flex h-7 items-center justify-between">
        {!collapsed && (
          <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-fg-muted">
            Agent
          </h3>
        )}
        <button
          type="button"
          onClick={toggleCollapsed}
          aria-expanded={!collapsed}
          aria-label={collapsed ? "Expand agent sidebar" : "Collapse agent sidebar"}
          title={collapsed ? "Expand agent sidebar" : "Collapse agent sidebar"}
          className={`inline-flex size-7 shrink-0 items-center justify-center rounded-md text-fg-3 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 ${collapsed ? "mx-auto" : "-mr-1"}`}
        >
          {collapsed ? (
            <ChevronDoubleRightIcon className="size-4" />
          ) : (
            <ChevronDoubleLeftIcon className="size-4" />
          )}
        </button>
      </div>

      <div className="mt-3 flex flex-col gap-4">
        {todoStats.total > 0 && (
          <CollapsibleSection
            sectionKey="todos"
            title="Todos"
            icon={ListBulletIcon}
            collapsed={collapsed}
            count={`${todoStats.done}/${todoStats.total}`}
            empty={false}
          >
            <TodoSection todos={todos} />
          </CollapsibleSection>
        )}

        <ContextWindowSection collapsed={collapsed} snapshot={contextWindow ?? null} />

        <CollapsibleSection
          sectionKey="skills"
          title="Skills"
          icon={CheckBadgeIcon}
          collapsed={collapsed}
          count={`${skills.activated.length}/${skills.available.length}`}
          empty={skills.available.length === 0}
          hideCountWhenCollapsed
        >
          <SkillsSection activated={skills.activated} available={skills.available} activatedNames={activatedSkillNames} />
        </CollapsibleSection>

        <CollapsibleSection
          sectionKey="mcps"
          title="MCPs"
          icon={ServerStackIcon}
          collapsed={collapsed}
          count={`${mcpServers.filter((s) => s.invoked).length}/${mcpServers.length}`}
          empty={mcpServers.length === 0}
          hideCountWhenCollapsed
        >
          <McpSection servers={mcpServers} />
        </CollapsibleSection>

        <CollapsibleSection
          sectionKey="tools"
          title="Tools"
          icon={WrenchScrewdriverIcon}
          collapsed={collapsed}
          count={`${invokedToolCount}/${agentTools.length}`}
          empty={agentTools.length === 0}
          hideCountWhenCollapsed
        >
          <AgentToolsSection tools={agentTools} />
        </CollapsibleSection>
      </div>
    </aside>
  );
}

// ---------- Collapsible section ----------

interface CollapsibleSectionProps {
  sectionKey: SectionKey;
  title: string;
  icon: IconType;
  collapsed: boolean;
  count: ReactNode;
  empty: boolean;
  /** Suppress the count badge under the icon when the sidebar is collapsed. */
  hideCountWhenCollapsed?: boolean;
  children: ReactNode;
}

function CollapsibleSection({
  sectionKey,
  title,
  icon: Icon,
  collapsed,
  count,
  empty,
  hideCountWhenCollapsed = false,
  children,
}: CollapsibleSectionProps) {
  const [open, setOpen] = useState(() => loadStoredSectionOpen(sectionKey));
  const toggle = useCallback(() => {
    setOpen((prev) => {
      const next = !prev;
      persistSectionOpen(sectionKey, next);
      return next;
    });
  }, [sectionKey]);

  if (collapsed) {
    return (
      <div className="flex flex-col items-center gap-0.5" title={`${title}: ${countLabel(count)}`}>
        <Icon className="size-4 shrink-0 text-fg-muted" />
        {!hideCountWhenCollapsed && (
          <span className="font-mono text-[10px] tabular-nums text-fg-3">{count}</span>
        )}
      </div>
    );
  }

  return (
    <div>
      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500"
      >
        <ChevronRightIcon
          className={`size-3 shrink-0 text-fg-muted transition-transform duration-150 ${open ? "rotate-90" : ""}`}
        />
        <Icon className="size-4 shrink-0 text-fg-muted" />
        <span className="flex-1 text-xs font-medium uppercase tracking-wider text-fg-3">{title}</span>
        <span className="font-mono text-xs tabular-nums text-fg-muted">{count}</span>
      </button>
      {open && (
        <div className="mt-1 pl-6 pr-2">
          {empty ? <p className="text-xs text-fg-muted">None</p> : children}
        </div>
      )}
    </div>
  );
}

// ---------- Todos ----------

interface TodoStats {
  done: number;
  total: number;
}

function countTodoStats(list: TodoListProjection | null): TodoStats {
  const items = list?.items ?? [];
  let done = 0;
  for (const item of items) {
    if (item.status === TodoStatus.COMPLETED) done += 1;
  }
  return { done, total: items.length };
}

function TodoSection({ todos }: { todos: TodoListProjection | null }) {
  if (!todos || (todos.items?.length ?? 0) === 0) return <p className="text-xs text-fg-muted">No todos.</p>;
  const items = Array.from(todos.items ?? []);
  items.sort((a, b) => a.order - b.order);
  return (
    <ul className="space-y-1">
      {items.map((item) => (
        <TodoRow key={item.id} todo={item} />
      ))}
    </ul>
  );
}

function TodoRow({ todo }: { todo: TodoProjection }) {
  const { Icon, color, srLabel, spin } = todoStatusVisual(todo.status);
  const muted = todo.status === TodoStatus.COMPLETED;
  return (
    <li className="flex items-start gap-1.5">
      <Icon className={`mt-0.5 size-3.5 shrink-0 ${color} ${spin ? "animate-spin" : ""}`} aria-label={srLabel} />
      <span className={`min-w-0 text-xs ${muted ? "text-fg-muted line-through" : "text-fg-2"}`}>{todo.subject}</span>
    </li>
  );
}

function todoStatusVisual(status: TodoStatus): { Icon: IconType; color: string; srLabel: string; spin?: boolean } {
  switch (status) {
    case TodoStatus.COMPLETED:
      return { Icon: CheckCircleIcon, color: "text-mint", srLabel: "Completed" };
    case TodoStatus.IN_PROGRESS:
      return { Icon: ArrowPathIcon, color: "text-teal-500", srLabel: "In progress", spin: true };
    case TodoStatus.DELETED:
      return { Icon: XCircleIcon, color: "text-fg-muted", srLabel: "Deleted" };
    case TodoStatus.PENDING:
    default:
      return { Icon: EmptyCircleIcon, color: "text-fg-muted", srLabel: "Pending" };
  }
}

/** Empty circle for pending/available states (matches Tailwind sizing). */
function EmptyCircleIcon({ className }: { className?: string }) {
  return (
    <span
      className={`inline-block rounded-full border border-current ${className ?? ""}`}
      aria-hidden="true"
    />
  );
}

// ---------- Context window ----------

interface ContextWindowSectionProps {
  collapsed: boolean;
  snapshot: StageContextWindow | null;
}

function ContextWindowSection({ collapsed, snapshot }: ContextWindowSectionProps) {
  const [open, setOpen] = useState(() => loadStoredSectionOpen("context"));
  const toggle = useCallback(() => {
    setOpen((prev) => {
      const next = !prev;
      persistSectionOpen("context", next);
      return next;
    });
  }, []);

  const pct = snapshot?.usage_percent ?? null;
  const pctLabel = pct == null ? "--" : `${Math.round(pct)}%`;

  if (collapsed) {
    return (
      <div className="flex flex-col items-center gap-0.5" title={`Context: ${pctLabel}`}>
        <Squares2X2Icon className="size-4 shrink-0 text-fg-muted" />
        <span className="font-mono text-[10px] tabular-nums text-fg-3">{pctLabel}</span>
      </div>
    );
  }

  return (
    <div>
      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500"
      >
        <ChevronRightIcon
          className={`size-3 shrink-0 text-fg-muted transition-transform duration-150 ${open ? "rotate-90" : ""}`}
        />
        <Squares2X2Icon className="size-4 shrink-0 text-fg-muted" />
        <span className="flex-1 text-xs font-medium uppercase tracking-wider text-fg-3">Context</span>
        <span className="font-mono text-xs tabular-nums text-fg-muted">{pctLabel}</span>
      </button>

      {open && (
        <>
          <div className="mt-2 px-2">
            <ContextBar snapshot={snapshot} />
          </div>
          <ContextBreakdown snapshot={snapshot} />
        </>
      )}
    </div>
  );
}

function ContextBar({ snapshot }: { snapshot: StageContextWindow | null }) {
  if (!snapshot || snapshot.usage_percent == null) {
    return (
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-overlay-strong" aria-hidden="true" />
    );
  }
  const breakdown = nonZeroBreakdown(snapshot.breakdown);
  const total = breakdown.reduce((acc, item) => acc + item.usage_percent, 0);
  const scale = total > 0 ? snapshot.usage_percent / total : 1;
  return (
    <>
      <meter
        min={0}
        max={100}
        value={Math.round(snapshot.usage_percent)}
        aria-label="Context window usage"
        className="sr-only"
      >
        {Math.round(snapshot.usage_percent)}%
      </meter>
      <div className="flex h-1.5 w-full overflow-hidden rounded-full bg-overlay-strong" aria-hidden="true">
        {breakdown.map((item) => (
          <span
            key={item.category}
            className="block h-full"
            style={{
              width:           `${item.usage_percent * scale}%`,
              backgroundColor: categoryColor(item.category),
            }}
          />
        ))}
      </div>
    </>
  );
}

function ContextBreakdown({ snapshot }: { snapshot: StageContextWindow | null }) {
  if (!snapshot) {
    return <p className="mt-2 px-2 text-xs text-fg-muted">Context usage not yet available.</p>;
  }
  if (snapshot.staleness === StageContextWindowStaleness.UNAVAILABLE) {
    return <p className="mt-2 px-2 text-xs text-fg-muted">Context usage unavailable for this stage.</p>;
  }
  const totalTokens = snapshot.input_tokens ?? 0;
  const contextWindow = snapshot.context_window_tokens ?? null;
  const breakdownRows = [];
  for (const item of snapshot.breakdown) {
    if (item.tokens > 0) breakdownRows.push(item);
  }
  return (
    <div className="mt-3 space-y-2 px-2">
      <div className="flex items-baseline justify-between font-mono text-xs tabular-nums text-fg-3">
        <span>{formatTokenCount(totalTokens, { compactDecimal: true })}</span>
        {contextWindow != null && (
          <span className="text-fg-muted">/ {formatTokenCount(contextWindow, { compactDecimal: true })}</span>
        )}
      </div>
      <ul className="space-y-1">
        {breakdownRows.map((item) => (
          <li key={item.category} className="flex items-center gap-2">
            <span
              className="block size-2 shrink-0 rounded-sm"
              style={{ backgroundColor: categoryColor(item.category) }}
              aria-hidden="true"
            />
            <span className="flex-1 truncate text-xs text-fg-3">{categoryLabel(item.category)}</span>
            <span className="font-mono text-xs tabular-nums text-fg-muted">
              {formatTokenCount(item.tokens, { compactDecimal: true })}
            </span>
          </li>
        ))}
      </ul>
      {snapshot.warnings.length > 0 && (
        <ul className="space-y-1">
          {snapshot.warnings.map((w) => (
            <li key={w.code} className="text-[11px] text-amber">⚠ {w.message}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function nonZeroBreakdown(items: StageContextWindowBreakdownItem[]): StageContextWindowBreakdownItem[] {
  return items.filter((i) => i.usage_percent > 0);
}

/**
 * Render the breakdown segment color via inline `backgroundColor` so the
 * value is independent of Tailwind's class scanner — every category resolves
 * to a known CSS custom property defined in `app.css`.
 *
 * Palette is chosen so the typical chunks (Conversation big + System +
 * Tools) read as three distinct hues rather than three adjacent teals.
 */
function categoryColor(category: StageContextWindowCategory): string {
  switch (category) {
    case StageContextWindowCategory.SYSTEM_PROMPT:
      return "var(--color-teal-700)";
    case StageContextWindowCategory.TOOLS:
      return "var(--color-amber)";
    case StageContextWindowCategory.MCP_TOOLS:
      return "var(--color-mint)";
    case StageContextWindowCategory.SKILLS:
      return "var(--color-teal-500)";
    case StageContextWindowCategory.MEMORY:
      return "var(--color-coral)";
    case StageContextWindowCategory.CONVERSATION:
      return "var(--color-teal-300)";
    case StageContextWindowCategory.OTHER:
    default:
      return "var(--color-fg-muted)";
  }
}

function categoryLabel(category: StageContextWindowCategory): string {
  switch (category) {
    case StageContextWindowCategory.SYSTEM_PROMPT:
      return "System prompt";
    case StageContextWindowCategory.TOOLS:
      return "Tools";
    case StageContextWindowCategory.MCP_TOOLS:
      return "MCP tools";
    case StageContextWindowCategory.SKILLS:
      return "Skills";
    case StageContextWindowCategory.MEMORY:
      return "Memory";
    case StageContextWindowCategory.CONVERSATION:
      return "Conversation";
    case StageContextWindowCategory.OTHER:
    default:
      return "Other";
  }
}

// ---------- Skills ----------

interface SkillsSectionProps {
  activated: ActivatedSkill[];
  available: AgentSkillSummary[];
  activatedNames: Set<string>;
}

function SkillsSection({ activated, available, activatedNames }: SkillsSectionProps) {
  if (activated.length === 0 && available.length === 0) {
    return <p className="text-xs text-fg-muted">No skills loaded.</p>;
  }
  const remaining = available.length - activatedNames.size;
  return (
    <div className="space-y-2">
      {activated.length > 0 && (
        <ul className="space-y-1">
          {activated.map((skill) => (
            <li key={`${skill.name}:${skill.source}`} className="flex items-center gap-1.5">
              <SkillSourceIcon source={skill.source} />
              <span className="min-w-0 flex-1 truncate text-xs text-fg-2">{skill.name}</span>
              <span className="text-[10px] uppercase tracking-wider text-fg-muted">{skill.source}</span>
            </li>
          ))}
        </ul>
      )}
      {remaining > 0 && (
        <p className="text-[11px] text-fg-muted">{`+${remaining} more available`}</p>
      )}
    </div>
  );
}

function SkillSourceIcon({ source }: { source: ActivatedSkill["source"] }) {
  const Icon = source === AgentSkillActivationSource.SLASH ? CommandLineIcon : PuzzlePieceIcon;
  return <Icon className="size-3.5 shrink-0 text-fg-muted" />;
}

// ---------- Tools ----------

function AgentToolsSection({ tools }: { tools: AgentToolSummary[] }) {
  if (tools.length === 0) return <p className="text-xs text-fg-muted">No tools reported.</p>;
  return (
    <ul className="space-y-1.5">
      {tools.map((tool) => {
        const nameClass = tool.invoked
          ? "min-w-0 flex-1 truncate text-xs text-fg-2"
          : "min-w-0 flex-1 truncate text-xs text-fg-muted";
        return (
          <li key={tool.name} title={tool.description} className="flex items-center gap-1.5">
            {tool.invoked ? (
              <CheckCircleIcon className="size-3.5 shrink-0 text-mint" aria-label="Used" />
            ) : (
              <EmptyCircleIcon className="size-3.5 shrink-0 text-fg-muted" aria-label="Not used" />
            )}
            <span className={nameClass}>{tool.name}</span>
          </li>
        );
      })}
    </ul>
  );
}

// ---------- MCPs ----------

function McpSection({ servers }: { servers: McpServerProjection[] }) {
  if (servers.length === 0) return <p className="text-xs text-fg-muted">No MCP servers.</p>;
  return (
    <ul className="space-y-1">
      {servers.map((server) => {
        // Dim unused servers so the eye lands on the invoked ones first;
        // failed servers stay coral regardless.
        const nameClass = server.status.kind === "ready" && !server.invoked
          ? "min-w-0 flex-1 truncate text-xs text-fg-muted"
          : "min-w-0 flex-1 truncate text-xs text-fg-2";
        return (
          <li key={server.server_name} className="flex items-center gap-1.5">
            {server.status.kind === "ready" ? (
              <CheckCircleIcon className="size-3.5 shrink-0 text-mint" aria-label="Ready" />
            ) : (
              <XCircleIcon className="size-3.5 shrink-0 text-coral" aria-label="Failed" />
            )}
            <span className={nameClass}>{server.server_name}</span>
            {server.status.kind === "ready" ? (
              <span className="font-mono text-[10px] tabular-nums text-fg-muted">
                {server.invoked
                  ? "used"
                  : `${server.tool_count} ${server.tool_count === 1 ? "tool" : "tools"}`}
              </span>
            ) : (
              <span className="text-[10px] uppercase tracking-wider text-coral">Failed</span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

// ---------- helpers ----------

type IconType = (props: { className?: string }) => ReactNode;

function countLabel(count: ReactNode): string {
  return typeof count === "string" || typeof count === "number" ? String(count) : "";
}

function loadStoredCollapsed(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(COLLAPSED_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

function persistCollapsed(collapsed: boolean) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(COLLAPSED_STORAGE_KEY, collapsed ? "1" : "0");
  } catch {
    // non-fatal
  }
}

function loadStoredSectionOpen(key: SectionKey): boolean {
  if (typeof window === "undefined") return SECTIONS_DEFAULT_OPEN[key];
  try {
    const stored = window.localStorage.getItem(SECTION_STORAGE_PREFIX + key);
    if (stored === "1") return true;
    if (stored === "0") return false;
  } catch {
    // fall through to default
  }
  return SECTIONS_DEFAULT_OPEN[key];
}

function persistSectionOpen(key: SectionKey, open: boolean) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(SECTION_STORAGE_PREFIX + key, open ? "1" : "0");
  } catch {
    // non-fatal
  }
}
