import { type ReactNode, useCallback, useState } from "react";
import {
  ChevronDoubleLeftIcon,
  ChevronDoubleRightIcon,
  ChevronRightIcon,
} from "@heroicons/react/20/solid";
import {
  ArrowPathIcon,
  CheckCircleIcon,
  ExclamationTriangleIcon,
  XCircleIcon,
} from "@heroicons/react/24/solid";
import {
  ArrowsRightLeftIcon,
  CheckBadgeIcon,
  CommandLineIcon,
  DocumentTextIcon,
  ListBulletIcon,
  PuzzlePieceIcon,
  ServerStackIcon,
  Squares2X2Icon,
  UserGroupIcon,
  WrenchScrewdriverIcon,
} from "@heroicons/react/24/outline";
import {
  ContextWindowCategory,
  ContextWindowStaleness,
  FailoverStop,
  SkillActivationSource,
  TodoStatus,
} from "@qltysh/fabro-api-client";
import type {
  AgentSessionActivatedSkill,
  AgentSessionCompaction,
  AgentSessionFailoverStop,
  AgentSessionMcpServer,
  AgentSessionProjection,
  AgentSessionRouteFailover,
  AgentSessionSubagent,
  ContextWindowBreakdownItem,
  SkillSummary,
  StageContextWindow,
  StageProjection,
  TodoListProjection,
  TodoProjection,
  ToolSummary,
} from "@qltysh/fabro-api-client";
import { formatTokenCount } from "../lib/format";

const COLLAPSED_STORAGE_KEY = "fabro:stage-insights-sidebar-collapsed";
const SECTION_STORAGE_PREFIX = "fabro:stage-insights-section:";

type SectionKey = "todos" | "context" | "files" | "subagents" | "tools" | "skills" | "mcps";

const SECTIONS_DEFAULT_OPEN: Record<SectionKey, boolean> = {
  todos:     true,
  context:   false,
  files:     false,
  subagents: false,
  tools:     false,
  skills:    false,
  mcps:      false,
};

export interface StageInsightsSidebarProps {
  /** Full stage projection (undefined while loading or for non-agent stages). */
  stage: StageProjection | undefined;
  /** Snapshot from `useRunStageContextWindow`. Null when unavailable. */
  contextWindow: StageContextWindow | null | undefined;
}

/**
 * The agent stage's sidebar. Everything about the agent's session comes from
 * `stage.agent`, the coding agent's own fold of the stage's events; the
 * stage itself contributes the tool catalog it was handed.
 */
export function StageInsightsSidebar({ stage, contextWindow }: StageInsightsSidebarProps) {
  const [collapsed, setCollapsed] = useState(loadStoredCollapsed);
  const toggleCollapsed = useCallback(() => {
    setCollapsed((prev) => {
      const next = !prev;
      persistCollapsed(next);
      return next;
    });
  }, []);

  const agent = stage?.agent ?? null;
  const todoLists = agent ? Object.values(agent.todos) : [];
  const todos = rootTodoList(agent);
  const otherTodoLists = todos ? todoLists.length - 1 : todoLists.length;
  const skills = agent?.skills ?? { activated: [], available: [] };
  const agentTools = stage?.agent_tools ?? [];
  const mcpServers = agent ? mcpServerRows(agent.mcp_servers) : [];
  const files = agent?.files_touched ?? [];
  const lastFile = agent?.last_file_touched ?? null;
  const subagents = agent?.subagents ?? [];
  const failovers = agent?.failovers ?? [];
  const failoverStopped = agent?.failover_stopped ?? null;
  const compactions = agent?.compactions ?? [];

  const todoStats = countTodoStats(todos);
  const activatedSkillNames = new Set(skills.activated.map((s) => s.name));
  const invokedToolCount = agentTools.filter((tool) => tool.invoked).length;
  const finishedSubagents = subagents.filter((s) => s.status.status !== "running").length;

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
          className={`inline-flex size-7 shrink-0 items-center justify-center rounded-md text-fg-3 transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current ${collapsed ? "mx-auto" : "-mr-1"}`}
        >
          {collapsed ? (
            <ChevronDoubleRightIcon className="size-4" />
          ) : (
            <ChevronDoubleLeftIcon className="size-4" />
          )}
        </button>
      </div>

      <FailoverBadge collapsed={collapsed} failovers={failovers} stopped={failoverStopped} />

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
            <TodoSection todos={todos} otherLists={otherTodoLists} />
          </CollapsibleSection>
        )}

        <ContextWindowSection
          collapsed={collapsed}
          snapshot={contextWindow ?? null}
          compactions={compactions}
        />

        <CollapsibleSection
          sectionKey="files"
          title="Files"
          icon={DocumentTextIcon}
          collapsed={collapsed}
          count={files.length}
          empty={files.length === 0}
          hideCountWhenCollapsed
        >
          <FilesSection files={files} lastFile={lastFile} />
        </CollapsibleSection>

        <CollapsibleSection
          sectionKey="subagents"
          title="Subagents"
          icon={UserGroupIcon}
          collapsed={collapsed}
          count={`${finishedSubagents}/${subagents.length}`}
          empty={subagents.length === 0}
          hideCountWhenCollapsed
        >
          <SubagentsSection subagents={subagents} />
        </CollapsibleSection>

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
        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current"
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

// ---------- Failover ----------

interface FailoverBadgeProps {
  collapsed: boolean;
  failovers: AgentSessionRouteFailover[];
  stopped: AgentSessionFailoverStop | null;
}

/**
 * Where the session's model moved: the last fallback route the root took,
 * and whether the prompt then stopped although routes were named. The
 * failures that caused each are the hover text.
 */
function FailoverBadge({ collapsed, failovers, stopped }: FailoverBadgeProps) {
  const last = failovers.length > 0 ? failovers[failovers.length - 1] : null;
  if (!last && !stopped) return null;
  const details = [
    ...failovers.map((move) => `${move.from} failed: ${move.error.message}`),
    stopped ? `${stopped.route}: ${stopped.error.message}` : null,
  ].filter((line): line is string => line != null);
  if (collapsed) {
    return (
      <div className="mt-2 flex justify-center" title={details.join("\n")}>
        <ArrowsRightLeftIcon className="size-4 shrink-0 text-amber" aria-label="Model failover" />
      </div>
    );
  }
  return (
    <div className="mt-2 space-y-0.5 px-2 text-[11px] text-amber" title={details.join("\n")}>
      {last && (
        <p className="flex items-center gap-1">
          <ArrowsRightLeftIcon className="size-3.5 shrink-0" aria-hidden="true" />
          <span className="min-w-0 truncate">
            {`Moved to ${last.to} after ${last.attempt} ${last.attempt === 1 ? "attempt" : "attempts"}`}
          </span>
        </p>
      )}
      {stopped && (
        <p className="pl-4.5">
          {`Stopped: ${stopped.reason === FailoverStop.EXHAUSTED ? "routes exhausted" : "failure not eligible for failover"}`}
        </p>
      )}
    </div>
  );
}

// ---------- Todos ----------

interface TodoStats {
  done: number;
  total: number;
}

/**
 * The root agent's own list: the one keyed by the root session's id. Subagent
 * plans are separate lists in the same map.
 */
function rootTodoList(agent: AgentSessionProjection | null): TodoListProjection | null {
  if (!agent) return null;
  const lists = Object.values(agent.todos);
  const rootId = agent.root_session_id;
  const root = rootId ? lists.find((list) => list.list_id.endsWith(rootId)) : undefined;
  return root ?? (lists.length === 1 ? lists[0] : null);
}

function countTodoStats(list: TodoListProjection | null): TodoStats {
  const items = list?.items ?? [];
  let done = 0;
  for (const item of items) {
    if (item.status === TodoStatus.COMPLETED) done += 1;
  }
  return { done, total: items.length };
}

function TodoSection({ todos, otherLists }: { todos: TodoListProjection | null; otherLists: number }) {
  if (!todos || (todos.items?.length ?? 0) === 0) return <p className="text-xs text-fg-muted">No todos.</p>;
  const items = Array.from(todos.items ?? []);
  items.sort((a, b) => a.order - b.order);
  return (
    <div className="space-y-2">
      <ul className="space-y-1">
        {items.map((item) => (
          <TodoRow key={item.id} todo={item} />
        ))}
      </ul>
      {otherLists > 0 && (
        <p className="text-[11px] text-fg-muted">
          {`+${otherLists} subagent ${otherLists === 1 ? "list" : "lists"}`}
        </p>
      )}
    </div>
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
  compactions: AgentSessionCompaction[];
}

function ContextWindowSection({ collapsed, snapshot, compactions }: ContextWindowSectionProps) {
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
        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current"
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
          <CompactionsRow compactions={compactions} />
        </>
      )}
    </div>
  );
}

/** How often the conversation was compacted, and what the last one kept. */
function CompactionsRow({ compactions }: { compactions: AgentSessionCompaction[] }) {
  if (compactions.length === 0) return null;
  const last = compactions[compactions.length - 1];
  const noun = compactions.length === 1 ? "compaction" : "compactions";
  return (
    <p className="mt-2 px-2 text-[11px] text-fg-muted" title={`Last compaction: ${last.reason}`}>
      {`${compactions.length} ${noun} · last kept ${last.preserved_turn_count} of ${last.original_turn_count} turns`}
    </p>
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
  if (snapshot.staleness === ContextWindowStaleness.UNAVAILABLE) {
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

function nonZeroBreakdown(items: ContextWindowBreakdownItem[]): ContextWindowBreakdownItem[] {
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
function categoryColor(category: ContextWindowCategory): string {
  switch (category) {
    case ContextWindowCategory.SYSTEM_PROMPT:
      return "var(--color-teal-700)";
    case ContextWindowCategory.TOOLS:
      return "var(--color-amber)";
    case ContextWindowCategory.MCP_TOOLS:
      return "var(--color-mint)";
    case ContextWindowCategory.SKILLS:
      return "var(--color-teal-500)";
    case ContextWindowCategory.MEMORY:
      return "var(--color-coral)";
    case ContextWindowCategory.CONVERSATION:
      return "var(--color-teal-300)";
    case ContextWindowCategory.OTHER:
    default:
      return "var(--color-fg-muted)";
  }
}

function categoryLabel(category: ContextWindowCategory): string {
  switch (category) {
    case ContextWindowCategory.SYSTEM_PROMPT:
      return "System prompt";
    case ContextWindowCategory.TOOLS:
      return "Tools";
    case ContextWindowCategory.MCP_TOOLS:
      return "MCP tools";
    case ContextWindowCategory.SKILLS:
      return "Skills";
    case ContextWindowCategory.MEMORY:
      return "Memory";
    case ContextWindowCategory.CONVERSATION:
      return "Conversation";
    case ContextWindowCategory.OTHER:
    default:
      return "Other";
  }
}

// ---------- Files ----------

/** Files the stage's agent and its subagents wrote or edited, sorted. */
function FilesSection({ files, lastFile }: { files: string[]; lastFile: string | null }) {
  if (files.length === 0) return <p className="text-xs text-fg-muted">No files written.</p>;
  return (
    <ul className="space-y-1">
      {files.map((path) => {
        const isLast = path === lastFile;
        return (
          <li key={path} title={path} className="flex items-center gap-1.5">
            <DocumentTextIcon className="size-3.5 shrink-0 text-fg-muted" aria-hidden="true" />
            <span className={`min-w-0 flex-1 truncate font-mono text-[11px] ${isLast ? "text-fg-2" : "text-fg-3"}`}>
              {fileLabel(path)}
            </span>
            {isLast && (
              <span className="text-[10px] uppercase tracking-wider text-fg-muted">last</span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/** The path from its last two segments, so a deep tree still reads. */
function fileLabel(path: string): string {
  const segments = path.split("/").filter((segment) => segment.length > 0);
  return segments.length <= 2 ? path : segments.slice(-2).join("/");
}

// ---------- Subagents ----------

function SubagentsSection({ subagents }: { subagents: AgentSessionSubagent[] }) {
  if (subagents.length === 0) return <p className="text-xs text-fg-muted">No subagents.</p>;
  return (
    <ul className="space-y-1">
      {subagents.map((subagent) => (
        <SubagentRow key={subagent.agent_id} subagent={subagent} />
      ))}
    </ul>
  );
}

function SubagentRow({ subagent }: { subagent: AgentSessionSubagent }) {
  const { status } = subagent;
  const title = status.status === "failed" ? `${subagent.task}\n${status.error.message}` : subagent.task;
  return (
    <li className="flex items-center gap-1.5" title={title}>
      <SubagentStatusIcon subagent={subagent} />
      <span className="min-w-0 flex-1 truncate text-xs text-fg-2">{subagent.task}</span>
      <SubagentStatusBadge subagent={subagent} />
    </li>
  );
}

function SubagentStatusIcon({ subagent }: { subagent: AgentSessionSubagent }) {
  const { status } = subagent;
  switch (status.status) {
    case "running":
      return <ArrowPathIcon className="size-3.5 shrink-0 animate-spin text-teal-500" aria-label="Running" />;
    case "completed":
      return status.success ? (
        <CheckCircleIcon className="size-3.5 shrink-0 text-mint" aria-label="Completed" />
      ) : (
        <ExclamationTriangleIcon className="size-3.5 shrink-0 text-amber" aria-label="Completed without success" />
      );
    case "failed":
      return <XCircleIcon className="size-3.5 shrink-0 text-coral" aria-label="Failed" />;
    case "closed":
      return <EmptyCircleIcon className="size-3.5 shrink-0 text-fg-muted" aria-label="Closed" />;
  }
}

function SubagentStatusBadge({ subagent }: { subagent: AgentSessionSubagent }) {
  const { status } = subagent;
  switch (status.status) {
    case "running":
      return <span className="text-[10px] uppercase tracking-wider text-teal-500">running</span>;
    case "completed":
      return (
        <span className="font-mono text-[10px] tabular-nums text-fg-muted">
          {`${status.turns_used} ${status.turns_used === 1 ? "turn" : "turns"}`}
        </span>
      );
    case "failed":
      return <span className="text-[10px] uppercase tracking-wider text-coral">Failed</span>;
    case "closed":
      return <span className="text-[10px] uppercase tracking-wider text-fg-muted">closed</span>;
  }
}

// ---------- Skills ----------

interface SkillsSectionProps {
  activated: AgentSessionActivatedSkill[];
  available: SkillSummary[];
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

function SkillSourceIcon({ source }: { source: AgentSessionActivatedSkill["source"] }) {
  const Icon = source === SkillActivationSource.SLASH ? CommandLineIcon : PuzzlePieceIcon;
  return <Icon className="size-3.5 shrink-0 text-fg-muted" />;
}

// ---------- Tools ----------

function AgentToolsSection({ tools }: { tools: ToolSummary[] }) {
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

type McpStatus = "ready" | "failed" | "disconnected";

interface McpServerRow {
  name: string;
  status: McpStatus;
  /** Why it failed, or what closed its connection. */
  error: string | null;
  toolCount: number;
  invoked: boolean;
}

/**
 * The agent's MCP servers as rows. A closed connection outranks a failed
 * start, which outranks ready; the tool count is the tools it advertised.
 */
function mcpServerRows(servers: Record<string, AgentSessionMcpServer>): McpServerRow[] {
  return Object.entries(servers).map(([name, server]) => {
    const status: McpStatus =
      server.disconnected != null ? "disconnected" : server.error != null ? "failed" : "ready";
    return {
      name,
      status,
      error:     server.disconnected ?? server.error ?? null,
      toolCount: server.tools.length,
      invoked:   server.invoked,
    };
  });
}

function McpSection({ servers }: { servers: McpServerRow[] }) {
  if (servers.length === 0) return <p className="text-xs text-fg-muted">No MCP servers.</p>;
  return (
    <ul className="space-y-1">
      {servers.map((server) => {
        // Dim unused servers so the eye lands on the invoked ones first;
        // failed and disconnected servers keep their tone regardless.
        const nameClass = server.status === "ready" && !server.invoked
          ? "min-w-0 flex-1 truncate text-xs text-fg-muted"
          : "min-w-0 flex-1 truncate text-xs text-fg-2";
        return (
          <li key={server.name} className="flex items-center gap-1.5" title={server.error ?? undefined}>
            <McpStatusIcon status={server.status} />
            <span className={nameClass}>{server.name}</span>
            <McpStatusBadge server={server} />
          </li>
        );
      })}
    </ul>
  );
}

function McpStatusIcon({ status }: { status: McpStatus }) {
  switch (status) {
    case "ready":
      return <CheckCircleIcon className="size-3.5 shrink-0 text-mint" aria-label="Ready" />;
    case "disconnected":
      return (
        <ExclamationTriangleIcon
          className="size-3.5 shrink-0 text-amber"
          aria-label="Disconnected"
        />
      );
    case "failed":
      return <XCircleIcon className="size-3.5 shrink-0 text-coral" aria-label="Failed" />;
  }
}

function McpStatusBadge({ server }: { server: McpServerRow }) {
  switch (server.status) {
    case "ready":
      return (
        <span className="font-mono text-[10px] tabular-nums text-fg-muted">
          {server.invoked
            ? "used"
            : `${server.toolCount} ${server.toolCount === 1 ? "tool" : "tools"}`}
        </span>
      );
    case "disconnected":
      return (
        <span className="text-[10px] uppercase tracking-wider text-amber">Disconnected</span>
      );
    case "failed":
      return <span className="text-[10px] uppercase tracking-wider text-coral">Failed</span>;
  }
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
