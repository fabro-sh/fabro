import { Dialog, DialogBackdrop, DialogPanel, Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import {
  Bars3Icon,
  ChevronDoubleLeftIcon,
  ChevronDoubleRightIcon,
  PlusIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { useId, useState, type CSSProperties, type ReactNode } from "react";
import { Link } from "react-router";

import { Tooltip } from "../components/ui";
import { FabroBrandMark, UserAvatar } from "../components/fabro-brand-mark";
import { useMediaQuery } from "../hooks/effects";
import { classNames } from "../lib/class-names";
import {
  createWorkflowHref,
  createWorkflowPurpose,
  isNavigationItemActive,
  navigation,
  type NavigationItem,
} from "./navigation";

const COLLAPSED_STORAGE_KEY = "fabro.sidebar.collapsed";
const WIDE_QUERY = "(min-width: 64rem)";
const DESKTOP_QUERY = "(min-width: 48rem)";

/**
 * The rail is its own ground: navy beside light pages in Factory's Signal palette.
 * Each role reads the theme's `--rail-*` token, then `--nav-*`, then the ordinary
 * surfaces, so any palette works without the sidebar knowing its name.
 */
const SIDEBAR_TOKENS = {
  "--sidebar-bg": "var(--rail, var(--nav-bg, var(--color-panel)))",
  "--sidebar-image": "var(--rail-image, none)",
  "--sidebar-line": "var(--rail-line, var(--nav-border, var(--color-line-strong)))",
  "--sidebar-text": "var(--rail-fg, var(--nav-text, var(--color-fg-3)))",
  "--sidebar-muted": "var(--rail-muted, var(--nav-muted, var(--color-fg-3)))",
  "--sidebar-strong": "var(--rail-strong, var(--nav-brand, var(--color-fg)))",
  "--sidebar-hover": "var(--rail-hover, var(--nav-hover, var(--color-overlay)))",
  "--sidebar-hover-text": "var(--rail-hover-fg, var(--nav-hover-text, var(--color-fg)))",
  "--sidebar-selected": "var(--rail-selected, var(--nav-selected, var(--color-overlay-strong)))",
  "--sidebar-selected-image": "var(--rail-selected-image, none)",
  "--sidebar-selected-text": "var(--rail-selected-fg, var(--nav-selected-text, var(--color-fg)))",
  "--sidebar-selected-rule": "var(--rail-selected-rule, var(--nav-selected-border, transparent))",
  "--sidebar-selected-shadow": "var(--rail-selected-shadow, 0 0 #0000)",
  "--sidebar-indicator": "var(--rail-indicator, var(--color-teal-500))",
  "--sidebar-ring": "var(--rail-ring, var(--nav-ring, var(--color-teal-500)))",
  "--sidebar-control": "var(--rail-control, transparent)",
  "--sidebar-control-text": "var(--rail-control-fg, var(--sidebar-text))",
  "--sidebar-control-line": "var(--rail-control-line, var(--sidebar-line))",
  "--sidebar-radius": "var(--radius-btn, var(--radius-md, 6px))",
} as CSSProperties;

const SIDEBAR_SURFACE =
  "border-[var(--sidebar-line)] bg-[var(--sidebar-bg)] [background-image:var(--sidebar-image)] text-[var(--sidebar-text)]";
const ROW = "rounded-[var(--sidebar-radius)] text-[13.5px] font-bold transition-colors";
const ROW_IDLE = "text-[var(--sidebar-text)] hover:bg-[var(--sidebar-hover)] hover:text-[var(--sidebar-hover-text)]";
const ROW_CURRENT =
  "bg-[var(--sidebar-selected)] [background-image:var(--sidebar-selected-image)] text-[var(--sidebar-selected-text)] [box-shadow:inset_0_0_0_1px_var(--sidebar-selected-rule),var(--sidebar-selected-shadow)]";
const FOCUS_RING =
  "focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current";
const SIGNAL =
  "bg-[var(--accent-pop,var(--color-teal-500))] text-[var(--accent-pop-fg,var(--color-on-primary))] [box-shadow:var(--e-signal,0_0_#0000)] hover:bg-[var(--accent-pop-hover,var(--color-teal-300))] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-current";

/** The mark on the palette's highlight tile, and the name beside it unless the rail is collapsed. */
function Brand({ compact, showWord }: { compact?: boolean; showWord: boolean }) {
  return (
    <>
      <FabroBrandMark className={compact ? "size-7" : "size-8"} />
      {showWord ? (
        <span
          className={classNames(
            "font-semibold text-[var(--sidebar-strong)] [font-family:var(--font-display,var(--font-sans))]",
            compact ? "text-[15px]" : "text-[17px]",
          )}
        >
          Fabro
        </span>
      ) : null}
    </>
  );
}

export interface ShellUser {
  name: string;
  email: string;
  avatarUrl: string;
}

interface SidebarProps {
  pathname: string;
  user: ShellUser;
  canSignOut: boolean;
  homeHref: string;
}

function readStoredCollapsed(): boolean | null {
  try {
    const stored = globalThis.localStorage.getItem(COLLAPSED_STORAGE_KEY);
    return stored === null ? null : stored === "true";
  } catch {
    // Storage blocked. The sidebar still works; it just won't remember.
    return null;
  }
}

/**
 * Expanded or rail. An explicit choice is remembered. With no choice yet, a
 * medium-width window starts on the rail, so the canvas keeps the room.
 */
export function useSidebarCollapsed(): { collapsed: boolean; toggle: () => void } {
  const isWide = useMediaQuery(WIDE_QUERY, true);
  const [stored, setStored] = useState(readStoredCollapsed);
  const collapsed = stored ?? !isWide;
  return {
    collapsed,
    toggle: () => {
      const next = !collapsed;
      setStored(next);
      try {
        globalThis.localStorage.setItem(COLLAPSED_STORAGE_KEY, String(next));
      } catch {
        // Storage blocked: this choice lasts until the page reloads.
      }
    },
  };
}

/** The destination and what it's for. Screen readers get the purpose via `aria-describedby`. */
function SidebarTooltip({ name, purpose, collapsed, children }: { name: string; purpose: string; collapsed: boolean; children: ReactNode }) {
  return (
    <Tooltip
      disabled={!collapsed}
      side="right"
      triggerClassName="flex w-full"
      label={
        <span className="flex max-w-56 flex-col">
          <span className="font-medium text-fg">{name}</span>
          <span className="text-fg-3">{purpose}</span>
        </span>
      }
    >
      {children}
    </Tooltip>
  );
}

function CreateLink({ collapsed, onNavigate }: { collapsed: boolean; onNavigate?: () => void }) {
  const purposeId = useId();
  return (
    <>
      <span id={purposeId} className="sr-only">
        {createWorkflowPurpose}
      </span>
      <SidebarTooltip name="Create workflow" purpose={createWorkflowPurpose} collapsed={collapsed}>
        <Link
          to={createWorkflowHref}
          aria-label="Create workflow"
          aria-describedby={purposeId}
          onClick={onNavigate}
          className={classNames(
            "flex h-9 w-full items-center rounded-[var(--sidebar-radius)] text-[13.5px] font-extrabold transition-colors",
            SIGNAL,
          )}
        >
          <span className="flex w-10 shrink-0 justify-center">
            <PlusIcon className="size-4" aria-hidden="true" />
          </span>
          {collapsed ? null : <span className="truncate">Create</span>}
        </Link>
      </SidebarTooltip>
    </>
  );
}

function NavLink({
  item,
  pathname,
  collapsed,
  onNavigate,
}: {
  item: NavigationItem;
  pathname: string;
  collapsed: boolean;
  onNavigate?: () => void;
}) {
  const purposeId = useId();
  const current = isNavigationItemActive(item, pathname);
  return (
    <li>
      <span id={purposeId} className="sr-only">
        {item.purpose}
      </span>
      <SidebarTooltip name={item.name} purpose={item.purpose} collapsed={collapsed}>
        <Link
          to={item.href}
          aria-current={current ? "page" : undefined}
          aria-describedby={purposeId}
          onClick={onNavigate}
          className={classNames(
            "relative flex h-9 w-full items-center",
            ROW,
            FOCUS_RING,
            current ? ROW_CURRENT : ROW_IDLE,
          )}
        >
          {current ? (
            <span className="absolute inset-y-1.5 left-0 w-[3px] rounded-full bg-[var(--sidebar-indicator)]" aria-hidden="true" />
          ) : null}
          {/* The icon column stays put, so nothing jumps when the rail expands. */}
          <span className="flex w-10 shrink-0 justify-center">
            <item.icon className="size-4" aria-hidden="true" />
          </span>
          <span className={collapsed ? "sr-only" : "truncate"}>{item.name}</span>
        </Link>
      </SidebarTooltip>
    </li>
  );
}

function UserMenu({ user, canSignOut, collapsed }: { user: ShellUser; canSignOut: boolean; collapsed: boolean }) {
  return (
    <Menu as="div">
      <SidebarTooltip name={user.name} purpose="Your profile, sessions and sign out." collapsed={collapsed}>
        <MenuButton
          className={classNames(
            "flex h-10 w-full items-center rounded-[var(--sidebar-radius)] text-left transition-colors",
            ROW_IDLE,
            FOCUS_RING,
          )}
        >
          <span className="sr-only">Open user menu</span>
          <span className="flex w-10 shrink-0 justify-center">
            <UserAvatar avatarUrl={user.avatarUrl} className="size-6" />
          </span>
          {collapsed ? null : (
            <span className="flex min-w-0 flex-1 flex-col pr-2">
              <span className="truncate text-[13px] font-bold text-[var(--sidebar-strong)]">{user.name}</span>
              <span className="truncate text-xs text-[var(--sidebar-muted)]">{user.email}</span>
            </span>
          )}
        </MenuButton>
      </SidebarTooltip>
      <MenuItems
        anchor={{ to: "right end", gap: 8 }}
        className="z-40 w-48 rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong"
      >
        <MenuItem>
          <Link to="/profile" className="block w-full px-4 py-2 text-left text-sm text-fg-3 data-focus:bg-overlay data-focus:outline-hidden">
            Profile
          </Link>
        </MenuItem>
        {canSignOut ? (
          <MenuItem>
            <form method="POST" action="/auth/logout">
              <button type="submit" className="block w-full px-4 py-2 text-left text-sm text-fg-3 data-focus:bg-overlay data-focus:outline-hidden">
                Sign out
              </button>
            </form>
          </MenuItem>
        ) : null}
      </MenuItems>
    </Menu>
  );
}

/** Desktop navigation: full labels, or an icon rail with the same destinations. */
export function DesktopSidebar({
  pathname,
  user,
  canSignOut,
  homeHref,
  collapsed,
  onToggle,
}: SidebarProps & { collapsed: boolean; onToggle: () => void }) {
  const isDesktop = useMediaQuery(DESKTOP_QUERY, true);
  // Unmount portal-based menus and tooltips when the mobile navigation takes over.
  if (!isDesktop) return null;
  const toggleLabel = collapsed ? "Expand navigation" : "Collapse navigation";
  const ToggleIcon = collapsed ? ChevronDoubleRightIcon : ChevronDoubleLeftIcon;
  const sidebarId = "app-sidebar";
  return (
    <aside
      id={sidebarId}
      aria-label="Main navigation"
      style={SIDEBAR_TOKENS}
      className={classNames(
        "sticky top-0 z-30 hidden h-dvh shrink-0 flex-col border-r transition-[width] duration-200 ease-out motion-reduce:transition-none md:flex",
        SIDEBAR_SURFACE,
        collapsed ? "w-14" : "w-62",
      )}
    >
      <div className="flex h-14 shrink-0 items-center px-3">
        <Link
          to={homeHref}
          aria-label="Fabro home"
          className={classNames("flex items-center gap-2.5 rounded-[var(--sidebar-radius)]", FOCUS_RING)}
        >
          <Brand showWord={!collapsed} />
        </Link>
      </div>
      <div className="shrink-0 px-2 pb-2">
        <CreateLink collapsed={collapsed} />
      </div>
      <nav aria-label="Main" className="min-h-0 flex-1 overflow-y-auto px-2 pt-2">
        <ul className="flex flex-col gap-1">
          {navigation
            .filter((item) => item.placement === "main")
            .map((item) => (
              <NavLink key={item.href} item={item} pathname={pathname} collapsed={collapsed} />
            ))}
        </ul>
      </nav>
      <div className="flex shrink-0 flex-col gap-1 border-t border-[var(--sidebar-line)] p-2">
        <ul className="flex flex-col gap-1">
          {navigation
            .filter((item) => item.placement === "footer")
            .map((item) => (
              <NavLink key={item.href} item={item} pathname={pathname} collapsed={collapsed} />
            ))}
        </ul>
        <UserMenu user={user} canSignOut={canSignOut} collapsed={collapsed} />
        <SidebarTooltip
          collapsed={collapsed}
          name={toggleLabel}
          purpose={collapsed ? "Show labels beside the icons." : "Shrink to icons and give the page more room."}
        >
          <button
            type="button"
            aria-label={toggleLabel}
            aria-expanded={!collapsed}
            aria-controls={sidebarId}
            onClick={onToggle}
            className={classNames(
              "flex h-9 w-full items-center",
              ROW,
              ROW_IDLE,
              FOCUS_RING,
            )}
          >
            <span className="flex w-10 shrink-0 justify-center">
              <ToggleIcon className="size-4" aria-hidden="true" />
            </span>
            {collapsed ? null : <span className="truncate">Collapse</span>}
          </button>
        </SidebarTooltip>
      </div>
    </aside>
  );
}

/**
 * Small screens: a slim bar over the page and a drawer behind the menu button, so
 * the canvas keeps nearly the whole screen. The dialog traps focus, closes on
 * Escape or a click outside, and hands focus back to the button that opened it.
 */
export function MobileNavigation({ pathname, user, canSignOut, homeHref }: SidebarProps) {
  const [open, setOpen] = useState(false);
  const isDesktop = useMediaQuery(DESKTOP_QUERY, true);
  const drawerId = "app-navigation-drawer";
  // Growing past the breakpoint leaves no drawer to return to. Without this, its
  // focus trap and scroll lock would outlive it.
  if (open && isDesktop) setOpen(false);
  const close = () => setOpen(false);

  return (
    <>
      <div style={SIDEBAR_TOKENS} className={`flex h-12 shrink-0 items-center gap-2 border-b px-2 md:hidden ${SIDEBAR_SURFACE}`}>
        <button
          type="button"
          aria-label="Open navigation"
          aria-expanded={open}
          aria-controls={drawerId}
          onClick={() => setOpen(true)}
          className={classNames(
            "inline-flex size-9 shrink-0 items-center justify-center rounded-[var(--sidebar-radius)] border border-[var(--sidebar-control-line)] bg-[var(--sidebar-control)] text-[var(--sidebar-control-text)]",
            FOCUS_RING,
          )}
        >
          <Bars3Icon className="size-5" aria-hidden="true" />
        </button>
        <Link
          to={homeHref}
          aria-label="Fabro home"
          className={classNames("flex items-center gap-2 rounded-[var(--sidebar-radius)]", FOCUS_RING)}
        >
          <Brand compact showWord />
        </Link>
        <Link
          to={createWorkflowHref}
          aria-label="Create workflow"
          className={classNames(
            "ml-auto inline-flex h-8 shrink-0 items-center gap-1 rounded-[var(--sidebar-radius)] px-3 text-[13px] font-extrabold transition-colors",
            SIGNAL,
          )}
        >
          Create
          <PlusIcon className="size-4" aria-hidden="true" />
        </Link>
      </div>

      <Dialog
        id={drawerId}
        open={open}
        onClose={close}
        aria-label="Main navigation"
        className="relative z-50 md:hidden"
      >
        <DialogBackdrop className="fixed inset-0 bg-backdrop" />
        <div className="fixed inset-y-0 left-0 flex w-72 max-w-[85vw]">
          <DialogPanel style={SIDEBAR_TOKENS} className={`flex w-full flex-col border-r ${SIDEBAR_SURFACE}`}>
            <div className="flex h-12 shrink-0 items-center justify-between px-2">
              <Link
                to={homeHref}
                aria-label="Fabro home"
                onClick={close}
                className={classNames("ml-1 flex items-center gap-2 rounded-[var(--sidebar-radius)]", FOCUS_RING)}
              >
                <Brand compact showWord />
              </Link>
              <button
                type="button"
                aria-label="Close navigation"
                onClick={close}
                className={classNames(
                  "inline-flex size-9 items-center justify-center rounded-[var(--sidebar-radius)]",
                  ROW_IDLE,
                  FOCUS_RING,
                )}
              >
                <XMarkIcon className="size-5" aria-hidden="true" />
              </button>
            </div>
            <div className="shrink-0 px-2 pb-2">
              <CreateLink collapsed={false} onNavigate={close} />
            </div>
            <nav aria-label="Main" className="min-h-0 flex-1 overflow-y-auto px-2 pt-2">
              <ul className="flex flex-col gap-1">
                {navigation.map((item) => {
                  const current = isNavigationItemActive(item, pathname);
                  return (
                    <li key={item.href}>
                      {/* Touch has no hover, so the purpose is written out. */}
                      <Link
                        to={item.href}
                        aria-current={current ? "page" : undefined}
                        onClick={close}
                        className={classNames(
                          "flex items-start rounded-[var(--sidebar-radius)] py-2 transition-colors",
                          FOCUS_RING,
                          current ? ROW_CURRENT : ROW_IDLE,
                        )}
                      >
                        <span className="flex w-10 shrink-0 justify-center pt-0.5">
                          <item.icon className="size-5" aria-hidden="true" />
                        </span>
                        <span className="flex min-w-0 flex-col pr-2">
                          <span className="text-[13.5px] font-bold">{item.name}</span>
                          <span className="text-xs font-normal opacity-80">{item.purpose}</span>
                        </span>
                      </Link>
                    </li>
                  );
                })}
              </ul>
            </nav>
            <div className="shrink-0 border-t border-[var(--sidebar-line)] p-2">
              <div className="flex items-center gap-2 px-2 py-2">
                <UserAvatar avatarUrl={user.avatarUrl} className="size-8" />
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium text-[var(--sidebar-strong)]">{user.name}</div>
                  <div className="truncate text-xs text-[var(--sidebar-muted)]">{user.email}</div>
                </div>
              </div>
              <Link
                to="/profile"
                onClick={close}
                className={classNames(
                  "block px-2 py-2",
                  ROW,
                  ROW_IDLE,
                  FOCUS_RING,
                )}
              >
                Profile
              </Link>
              {canSignOut ? (
                <form method="POST" action="/auth/logout">
                  <button
                    type="submit"
                    className={classNames(
                      "block w-full px-2 py-2 text-left",
                      ROW,
                      ROW_IDLE,
                      FOCUS_RING,
                    )}
                  >
                    Sign out
                  </button>
                </form>
              ) : null}
            </div>
          </DialogPanel>
        </div>
      </Dialog>
    </>
  );
}
