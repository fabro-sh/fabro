import {
  BoltIcon,
  CircleStackIcon,
  Cog6ToothIcon,
  CpuChipIcon,
  KeyIcon,
  PlusIcon,
  PuzzlePieceIcon,
  ShieldCheckIcon,
} from "@heroicons/react/24/outline";
import { Link, Outlet, useLocation, useMatches } from "react-router";

export function meta({}: any) {
  return [{ title: "Settings — Fabro" }];
}

export const handle = { hideHeader: true };

type NavItem = {
  type?: "link";
  name: string;
  href: string;
  icon: typeof Cog6ToothIcon;
  match: (pathname: string) => boolean;
};

type NavDivider = { type: "divider"; key: string };

type NavEntry = NavItem | NavDivider;

const navItems: NavEntry[] = [
  {
    name: "General",
    href: "/settings",
    icon: Cog6ToothIcon,
    match: (p) => p === "/settings",
  },
  {
    name: "Integrations",
    href: "/settings/integrations",
    icon: PuzzlePieceIcon,
    match: (p) => p.startsWith("/settings/integrations"),
  },
  {
    name: "Models",
    href: "/settings/models",
    icon: CpuChipIcon,
    match: (p) => p.startsWith("/settings/models"),
  },
  {
    name: "Secrets",
    href: "/settings/secrets",
    icon: KeyIcon,
    match: (p) => p.startsWith("/settings/secrets"),
  },
  {
    name: "Security",
    href: "/settings/security",
    icon: ShieldCheckIcon,
    match: (p) => p.startsWith("/settings/security"),
  },
  {
    name: "Storage",
    href: "/settings/storage",
    icon: CircleStackIcon,
    match: (p) => p.startsWith("/settings/storage"),
  },
  { type: "divider", key: "after-storage" },
  {
    name: "Live Events",
    href: "/settings/live-events",
    icon: BoltIcon,
    match: (p) => p.startsWith("/settings/live-events"),
  },
];

function isLink(entry: NavEntry): entry is NavItem {
  return entry.type !== "divider";
}

// A settings page can declare a header action and description by exporting
// `handle = { headerAction: { to, label }, description }`. When a description
// is present the layout renders the full header — title, description, and
// action — as one row, with the action centered against both lines.
type HeaderAction = { to: string; label: string };

function readHeaderAction(handle: unknown): HeaderAction | null {
  if (!handle || typeof handle !== "object" || !("headerAction" in handle)) {
    return null;
  }
  const action = handle.headerAction;
  if (!action || typeof action !== "object") return null;
  if (!("to" in action) || !("label" in action)) return null;
  const { to, label } = action;
  if (typeof to !== "string" || typeof label !== "string") return null;
  return { to, label };
}

function readDescription(handle: unknown): string | null {
  if (!handle || typeof handle !== "object" || !("description" in handle)) {
    return null;
  }
  const description = handle.description;
  return typeof description === "string" ? description : null;
}

function classNames(...classes: Array<string | false | null | undefined>) {
  return classes.filter(Boolean).join(" ");
}

export default function SettingsLayout() {
  const { pathname } = useLocation();
  const matches = useMatches();
  const currentName =
    navItems.filter(isLink).find((item) => item.match(pathname))?.name ?? "Settings";
  const fullHeight = matches.some(
    (m) => (m.handle as { fullHeight?: boolean } | undefined)?.fullHeight,
  );
  const headerAction =
    matches.map((m) => readHeaderAction(m.handle)).find((a) => a !== null) ?? null;
  const description =
    matches.map((m) => readDescription(m.handle)).find((d) => d !== null) ?? null;

  return (
    <div
      className={classNames(
        "flex flex-col gap-6 lg:flex-row",
        fullHeight && "min-h-0 flex-1",
      )}
    >
      <aside className="lg:w-56 lg:shrink-0">
        <nav className="sticky top-6">
          <ul role="list" className="flex gap-1 overflow-x-auto lg:flex-col lg:gap-0.5">
            {navItems.map((entry) => {
              if (!isLink(entry)) {
                return (
                  <li
                    key={entry.key}
                    role="separator"
                    aria-orientation="vertical"
                    className="mx-1 self-stretch border-l border-line lg:mx-0 lg:my-2 lg:self-auto lg:border-l-0 lg:border-t"
                  />
                );
              }
              const current = entry.match(pathname);
              return (
                <li key={entry.name}>
                  <Link
                    to={entry.href}
                    aria-current={current ? "page" : undefined}
                    className={classNames(
                      "flex items-center gap-2 rounded-md px-2.5 py-2 text-sm whitespace-nowrap transition-colors",
                      current
                        ? "bg-overlay text-fg"
                        : "text-fg-3 hover:bg-overlay hover:text-fg",
                    )}
                  >
                    <entry.icon className="size-4 shrink-0" aria-hidden="true" />
                    {entry.name}
                  </Link>
                </li>
              );
            })}
          </ul>
        </nav>
      </aside>

      <div
        className={classNames(
          "min-w-0 flex-1",
          fullHeight && "flex min-h-0 flex-col",
        )}
      >
        <div
          className={classNames(
            "flex items-center justify-between gap-6",
            description ? "mb-6" : "mb-2",
          )}
        >
          <div className="min-w-0">
            <h1 className="text-xl font-semibold tracking-tight text-fg">
              {currentName}
            </h1>
            {description ? (
              <p className="mt-1 max-w-[64ch] text-sm/6 text-fg-3 text-pretty">
                {description}
              </p>
            ) : null}
          </div>
          {headerAction ? (
            <Link
              to={headerAction.to}
              className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-line bg-panel/80 px-2.5 py-1 text-sm font-medium text-fg-3 transition-colors hover:border-line-strong hover:bg-panel hover:text-fg"
            >
              <PlusIcon className="size-3.5" aria-hidden="true" />
              {headerAction.label}
            </Link>
          ) : null}
        </div>
        <Outlet />
      </div>
    </div>
  );
}
