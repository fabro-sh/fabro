import {
  Disclosure,
  DisclosureButton,
  DisclosurePanel,
  Menu,
  MenuButton,
  MenuItem,
  MenuItems,
} from "@headlessui/react";
import {
  Bars3Icon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { Link, Outlet, useLocation, useMatches } from "react-router";
import { ErrorState } from "../components/state";
import { ToastProvider } from "../components/toast";
import { AskFabroLayoutProvider, useAskFabroLayout } from "../lib/ask-fabro-layout";
import { DemoModeProvider } from "../lib/demo-mode";
import { useAuthMe } from "../lib/queries";
import { allNavigation, getVisibleNavigation } from "./navigation";

function activeFor(item: (typeof allNavigation)[number], pathname: string): boolean {
  return pathname.startsWith(item.href);
}

function classNames(...classes: Array<string | false | null | undefined>) {
  return classes.filter(Boolean).join(" ");
}

export default function AppShell() {
  const { data: auth, error, isLoading } = useAuthMe();
  const { pathname } = useLocation();
  const matches = useMatches();

  if (isLoading && !auth) {
    return <div className="min-h-full bg-page" />;
  }

  if (error || !auth) {
    return (
      <div className="min-h-full bg-page py-12">
        <ErrorState
          title="Couldn't load your session"
          description="Refresh the page or sign in again."
        />
      </div>
    );
  }

  const { user, provider, demoMode } = auth;
  const navigation = getVisibleNavigation(demoMode);
  const currentNav = navigation.find((item) => activeFor(item, pathname));
  const title = currentNav?.name ?? "";
  const lastMatch = matches[matches.length - 1];
  const handle = lastMatch?.handle as { headerExtra?: React.ReactNode } | undefined;
  const headerExtra = handle?.headerExtra;
  const hideHeader = matches.some((m) => (m.handle as { hideHeader?: boolean } | undefined)?.hideHeader);
  const wide = matches.some((m) => (m.handle as { wide?: boolean } | undefined)?.wide);
  const fullHeight = matches.some(
    (m) => (m.handle as { fullHeight?: boolean } | undefined)?.fullHeight,
  );
  const maxWidth = wide ? "" : "max-w-5xl";

  return (
    <DemoModeProvider value={demoMode}>
    <ToastProvider>
    <AskFabroLayoutProvider>
    <div
      className={classNames(
        "isolate",
        fullHeight ? "flex h-dvh flex-col" : "min-h-full",
      )}
    >
      <Disclosure
        as="nav"
        className={classNames("bg-panel", fullHeight && "shrink-0")}
      >
        <div className="px-4 sm:px-6 lg:px-8">
          <div className="flex h-16 items-center justify-between">
            <div className="flex items-center">
              <div className="shrink-0">
                <Link to={demoMode ? "/start" : "/runs"}>
                  <img alt="Fabro" src="/images/logotype.svg" className="h-8 w-auto" />
                </Link>
              </div>
              <div className="hidden md:block">
                <div className="ml-10 flex items-baseline gap-x-4">
                  {navigation.map((item) => {
                    const current = activeFor(item, pathname);
                    return (
                      <Link
                        key={item.name}
                        to={item.href}
                        aria-current={current ? "page" : undefined}
                        className={classNames(
                          current
                            ? "bg-page/50 text-fg"
                            : "text-fg-3 hover:bg-overlay hover:text-fg",
                          "inline-flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium",
                        )}
                      >
                        <item.icon className="size-4" aria-hidden="true" />
                        {item.name}
                      </Link>
                    );
                  })}
                </div>
              </div>
            </div>
            <div className="hidden md:block">
              <div className="ml-4 flex items-center gap-3 md:ml-6">
                <Menu as="div" className="relative">
                  <MenuButton className="relative flex max-w-xs items-center rounded-full focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500">
                    <span className="absolute -inset-1.5" />
                    <span className="sr-only">Open user menu</span>
                    <img
                      alt=""
                      src={user.avatarUrl}
                      className="size-8 rounded-full outline -outline-offset-1 outline-line-strong"
                    />
                  </MenuButton>

                  <MenuItems
                    transition
                    className="absolute right-0 z-10 mt-2 w-48 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:transform data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
                  >
                    <MenuItem>
                      <Link
                        to="/profile"
                        className="block w-full px-4 py-2 text-left text-sm text-fg-3 data-focus:bg-overlay data-focus:outline-hidden"
                      >
                        Profile
                      </Link>
                    </MenuItem>
                    <MenuItem>
                      <form method="POST" action="/auth/logout">
                        <button
                          type="submit"
                          className="block w-full px-4 py-2 text-left text-sm text-fg-3 data-focus:bg-overlay data-focus:outline-hidden"
                        >
                          Sign out
                        </button>
                      </form>
                    </MenuItem>
                  </MenuItems>
                </Menu>
              </div>
            </div>
            <div className="-mr-2 flex md:hidden">
              <DisclosureButton className="group relative inline-flex items-center justify-center rounded-md p-2 text-fg-muted hover:bg-overlay hover:text-fg focus:outline-2 focus:outline-offset-2 focus:outline-teal-500">
                <span className="absolute -inset-0.5" />
                <span className="sr-only">Open main menu</span>
                <Bars3Icon
                  aria-hidden="true"
                  className="block size-6 group-data-open:hidden"
                />
                <XMarkIcon
                  aria-hidden="true"
                  className="hidden size-6 group-data-open:block"
                />
              </DisclosureButton>
            </div>
          </div>
        </div>

        <DisclosurePanel className="md:hidden">
          <div className="space-y-1 px-2 pt-2 pb-3 sm:px-3">
            {navigation.map((item) => {
              const current = activeFor(item, pathname);
              return (
                <DisclosureButton
                  key={item.name}
                  as={Link}
                  to={item.href}
                  aria-current={current ? "page" : undefined}
                  className={classNames(
                    current
                      ? "bg-page text-fg"
                      : "text-fg-3 hover:bg-overlay hover:text-fg",
                    "flex items-center gap-2 rounded-md px-3 py-2 text-base font-medium",
                  )}
                >
                  <item.icon className="size-5" aria-hidden="true" />
                  {item.name}
                </DisclosureButton>
              );
            })}
          </div>
          <div className="border-t border-line-strong pt-4 pb-3">
            <div className="flex items-center px-5">
              <div className="shrink-0">
                <img
                  alt=""
                  src={user.avatarUrl}
                  className="size-10 rounded-full outline -outline-offset-1 outline-line-strong"
                />
              </div>
              <div className="ml-3">
                <div className="text-base font-medium text-fg">
                  {user.name}
                </div>
                <div className="text-sm font-medium text-fg-muted">
                  {user.email}
                </div>
              </div>
            </div>
            <div className="mt-3 space-y-1 px-2">
              <DisclosureButton
                as={Link}
                to="/profile"
                className="block w-full rounded-md px-3 py-2 text-left text-base font-medium text-fg-muted hover:bg-overlay hover:text-fg"
              >
                Profile
              </DisclosureButton>
              {provider !== "tailscale" && (
                <form method="POST" action="/auth/logout">
                  <DisclosureButton
                    as="button"
                    type="submit"
                    className="block w-full rounded-md px-3 py-2 text-left text-base font-medium text-fg-muted hover:bg-overlay hover:text-fg"
                  >
                    Sign out
                  </DisclosureButton>
                </form>
              )}
            </div>
          </div>
        </DisclosurePanel>
      </Disclosure>

      {!hideHeader && (
        <header
          className={classNames(
            "relative bg-panel after:pointer-events-none after:absolute after:inset-x-0 after:bottom-0 after:border-b after:border-line-strong",
            fullHeight && "shrink-0",
          )}
        >
          <div className={`mx-auto ${maxWidth} px-4 py-4 sm:px-6 lg:px-8`}>
            <div className="flex items-center">
              <h1 className="text-xl font-semibold tracking-tight text-fg">
                {title}
              </h1>
              {headerExtra && <div className="ml-auto">{headerExtra}</div>}
            </div>
          </div>
        </header>
      )}
      <ShellMain fullHeight={fullHeight} maxWidth={maxWidth} />
    </div>
    </AskFabroLayoutProvider>
    </ToastProvider>
    </DemoModeProvider>
  );
}

/**
 * The scrollable content region. Inset on the right by the docked Ask Fabro
 * sidebar's width so opening the sidebar shifts the page content left rather
 * than covering it.
 */
function ShellMain({
  fullHeight,
  maxWidth,
}: {
  fullHeight: boolean;
  maxWidth: string;
}) {
  const { sidebarWidth, isResizing } = useAskFabroLayout();
  return (
    <main
      className={classNames(
        !isResizing &&
          "transition-[padding] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)]",
        fullHeight && "min-h-0 flex-1",
      )}
      style={{ paddingRight: sidebarWidth }}
    >
      <div
        className={classNames(
          `mx-auto ${maxWidth} px-4 py-6 sm:px-6 lg:px-8`,
          fullHeight && "box-border flex h-full min-h-0 flex-col",
        )}
      >
        <Outlet />
      </div>
    </main>
  );
}
