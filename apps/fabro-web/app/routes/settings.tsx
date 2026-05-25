import {
  BoltIcon,
  ChartBarSquareIcon,
  CircleStackIcon,
  Cog6ToothIcon,
  CpuChipIcon,
  CubeTransparentIcon,
  KeyIcon,
  PuzzlePieceIcon,
  ShieldCheckIcon,
} from "@heroicons/react/24/outline";
import { Fragment } from "react";
import { Link, Outlet, useLocation, useMatches } from "react-router";

export function meta({}: any) {
  return [{ title: "Settings — Fabro" }];
}

export const handle = { hideHeader: true };

export type NavItem = {
  name: string;
  href: string;
  icon: typeof Cog6ToothIcon;
  description: string;
  match: (pathname: string) => boolean;
};

export type NavSection = {
  key: string;
  label?: string;
  items: NavItem[];
};

export const navSections: NavSection[] = [
  {
    key: "general",
    label: "General",
    items: [
      {
        name: "Models",
        href: "/settings/models",
        icon: CpuChipIcon,
        description: "LLM providers and credentials.",
        match: (p) => p.startsWith("/settings/models"),
      },
      {
        name: "Integrations",
        href: "/settings/integrations",
        icon: PuzzlePieceIcon,
        description: "Slack, GitHub, and other services.",
        match: (p) => p.startsWith("/settings/integrations"),
      },
      {
        name: "Sandboxes",
        href: "/settings/sandboxes",
        icon: CubeTransparentIcon,
        description: "Where workflow stages execute.",
        match: (p) => p.startsWith("/settings/sandboxes"),
      },
      {
        name: "Security",
        href: "/settings/security",
        icon: ShieldCheckIcon,
        description: "Authentication and network allowlist.",
        match: (p) => p.startsWith("/settings/security"),
      },
      {
        name: "Secrets",
        href: "/settings/secrets",
        icon: KeyIcon,
        description: "Write-only values for workflow runs.",
        match: (p) => p.startsWith("/settings/secrets"),
      },
    ],
  },
  {
    key: "administration",
    label: "Administration",
    items: [
      {
        name: "Server",
        href: "/settings/server",
        icon: Cog6ToothIcon,
        description: "URLs, listen address, scheduler.",
        match: (p) => p.startsWith("/settings/server"),
      },
      {
        name: "Storage",
        href: "/settings/storage",
        icon: CircleStackIcon,
        description: "Database, runs, and artifacts.",
        match: (p) => p.startsWith("/settings/storage"),
      },
      {
        name: "Monitoring",
        href: "/settings/monitoring",
        icon: ChartBarSquareIcon,
        description: "CPU, memory, disk, concurrency.",
        match: (p) => p.startsWith("/settings/monitoring"),
      },
    ],
  },
  {
    key: "diagnostics",
    items: [
      {
        name: "Live Events",
        href: "/settings/live-events",
        icon: BoltIcon,
        description: "Real-time event stream.",
        match: (p) => p.startsWith("/settings/live-events"),
      },
    ],
  },
];

const allItems = navSections.flatMap((s) => s.items);

function classNames(...classes: Array<string | false | null | undefined>) {
  return classes.filter(Boolean).join(" ");
}

export default function SettingsLayout() {
  const { pathname } = useLocation();
  const matches = useMatches();
  const currentName =
    allItems.find((item) => item.match(pathname))?.name ?? "Settings";
  const fullHeight = matches.some(
    (m) => (m.handle as { fullHeight?: boolean } | undefined)?.fullHeight,
  );

  return (
    <div
      className={classNames(
        "flex flex-col gap-6 lg:flex-row",
        fullHeight && "min-h-0 flex-1",
      )}
    >
      <aside className="lg:w-56 lg:shrink-0">
        <nav className="sticky top-6">
          <ul className="flex gap-1 overflow-x-auto lg:flex-col lg:gap-0.5">
            {navSections.map((section, sectionIdx) => (
              <Fragment key={section.key}>
                {section.label ? (
                  <li
                    className={classNames(
                      "hidden lg:block lg:px-2.5 lg:pb-1 lg:text-xs lg:font-medium lg:uppercase lg:tracking-wider lg:text-fg-muted",
                      sectionIdx === 0 ? "lg:pt-0" : "lg:pt-4",
                    )}
                  >
                    {section.label}
                  </li>
                ) : sectionIdx > 0 ? (
                  <li
                    aria-hidden="true"
                    className="mx-1 self-stretch border-l border-line lg:mx-0 lg:my-2 lg:self-auto lg:border-l-0 lg:border-t"
                  />
                ) : null}
                {section.items.map((item) => {
                  const current = item.match(pathname);
                  return (
                    <li key={item.href}>
                      <Link
                        to={item.href}
                        aria-current={current ? "page" : undefined}
                        className={classNames(
                          "flex items-center gap-2 rounded-md px-2.5 py-2 text-sm whitespace-nowrap transition-colors",
                          current
                            ? "bg-overlay text-fg"
                            : "text-fg-3 hover:bg-overlay hover:text-fg",
                        )}
                      >
                        <item.icon className="size-4 shrink-0" aria-hidden="true" />
                        {item.name}
                      </Link>
                    </li>
                  );
                })}
              </Fragment>
            ))}
          </ul>
        </nav>
      </aside>

      <div
        className={classNames(
          "min-w-0 flex-1",
          fullHeight && "flex min-h-0 flex-col",
        )}
      >
        <h1 className="mb-2 text-xl font-semibold tracking-tight text-fg">
          {currentName}
        </h1>
        <Outlet />
      </div>
    </div>
  );
}
