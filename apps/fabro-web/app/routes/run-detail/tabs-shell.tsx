import { Link, Outlet, type UIMatch } from "react-router";

import { classNames } from "./model";

interface RunDetailTabDefinition {
  name: string;
  path: string;
  count: number | null;
  requiresSandbox?: boolean;
}

const allTabs: RunDetailTabDefinition[] = [
  { name: "Overview", path: "", count: null },
  { name: "Stages", path: "/stages", count: null },
  { name: "Files Changed", path: "/files", count: null },
  { name: "Children", path: "/children", count: null },
  { name: "Sandbox", path: "/sandbox", count: null, requiresSandbox: true },
  { name: "Billing", path: "/billing", count: null },
];

export type RunDetailTab = RunDetailTabDefinition;

export function runHasSandbox(runState: unknown): boolean {
  return !!(
    runState &&
    typeof runState === "object" &&
    "sandbox" in runState &&
    (runState as { sandbox?: unknown }).sandbox
  );
}

export function buildRunDetailTabs({
  hasSandbox,
  filesCount,
  childrenCount,
}: {
  hasSandbox: boolean;
  filesCount: number | null;
  childrenCount: number | null;
}) {
  const tabs: RunDetailTab[] = [];
  for (const tab of allTabs) {
    if (tab.requiresSandbox && !hasSandbox) continue;
    if (tab.name === "Files Changed") {
      tabs.push({ ...tab, count: filesCount });
    } else if (tab.name === "Children") {
      tabs.push({ ...tab, count: childrenCount });
    } else {
      tabs.push(tab);
    }
  }
  return tabs;
}

export function childRouteLayoutFlags(matches: UIMatch[]) {
  return {
    fullHeight: matches.some(
      (m) => (m.handle as { fullHeight?: boolean } | undefined)?.fullHeight,
    ),
    hideSteerBar: matches.some(
      (m) => (m.handle as { hideSteerBar?: boolean } | undefined)?.hideSteerBar,
    ),
  };
}

export function RunDetailTabsAndOutlet({
  tabs,
  basePath,
  pathname,
  fullHeight,
  hideSteerBar,
  hasPendingQuestions,
}: {
  tabs: RunDetailTab[];
  basePath: string;
  pathname: string;
  fullHeight: boolean;
  hideSteerBar: boolean;
  hasPendingQuestions: boolean;
}) {
  return (
    <>
      <div
        className={classNames(
          "relative before:pointer-events-none before:absolute before:bottom-0 before:left-1/2 before:h-px before:w-screen before:-translate-x-1/2 before:bg-line",
          fullHeight && "shrink-0",
        )}
      >
        <nav className="-mb-px flex gap-6">
          {tabs.map((tab) => {
            const tabPath = `${basePath}${tab.path}`;
            const isActive = tab.name === "Stages"
              ? pathname.startsWith(`${basePath}/stages`)
              : pathname === tabPath;
            return (
              <Link
                key={tab.name}
                to={tabPath}
                className={`border-b-2 pb-3.5 text-sm font-medium transition-colors ${
                  isActive
                    ? "border-teal-500 text-fg"
                    : "border-transparent text-fg-muted hover:border-line-strong hover:text-fg-3"
                }`}
              >
                {tab.name}
                {tab.count != null && tab.count > 0 && (
                  <span className={`ml-1.5 rounded-full px-1.5 py-0.5 text-xs font-normal tabular-nums ${
                    isActive ? "bg-overlay-strong text-fg-3" : "bg-overlay text-fg-muted"
                  }`}>
                    {tab.count}
                  </span>
                )}
              </Link>
            );
          })}
        </nav>
      </div>

      <div
        className={
          fullHeight
            ? "pt-3 flex min-h-0 flex-1 flex-col"
            : hideSteerBar && !hasPendingQuestions
              ? "pt-3"
              : "pt-3 pb-[var(--fabro-interview-dock-clearance)]"
        }
      >
        <Outlet />
      </div>
    </>
  );
}
