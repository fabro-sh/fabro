import { Outlet, useLocation, useMatches } from "react-router";
import { ErrorState } from "../components/state";
import { TooltipProvider } from "../components/ui";
import { classNames } from "../lib/class-names";
import { DemoModeProvider } from "../lib/demo-mode";
import { useAuthMe } from "../lib/queries";
import { isNavigationItemActive, navigation } from "./navigation";
import { DesktopSidebar, MobileNavigation, useSidebarCollapsed } from "./sidebar";

export default function AppShell() {
  const { data: auth, error, isLoading } = useAuthMe();
  const { pathname } = useLocation();
  const matches = useMatches();
  const sidebar = useSidebarCollapsed();

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
  const currentNav = navigation.find((item) => isNavigationItemActive(item, pathname));
  const title = currentNav?.name ?? "";
  const lastMatch = matches[matches.length - 1];
  const handle = lastMatch?.handle as { headerExtra?: React.ReactNode } | undefined;
  const headerExtra = handle?.headerExtra;
  const hideHeader = matches.some((m) => (m.handle as { hideHeader?: boolean } | undefined)?.hideHeader);
  const hideTitle = matches.some((m) => (m.handle as { hideTitle?: boolean } | undefined)?.hideTitle);
  const wide = matches.some((m) => (m.handle as { wide?: boolean } | undefined)?.wide);
  const fullHeight = matches.some(
    (m) => (m.handle as { fullHeight?: boolean } | undefined)?.fullHeight,
  );
  const maxWidth = wide ? "" : "max-w-5xl";

  const shellProps = {
    pathname,
    user,
    canSignOut: provider !== "tailscale",
    homeHref: demoMode ? "/start" : "/runs",
  };

  return (
    <DemoModeProvider value={demoMode}>
    <TooltipProvider>
    <div
      className={classNames(
        // Fixed-position page elements read these to stay clear of the navigation.
        "isolate flex [--fabro-nav-width:0px] [--fabro-topbar-height:3rem] md:[--fabro-topbar-height:0px]",
        sidebar.collapsed ? "md:[--fabro-nav-width:3.5rem]" : "md:[--fabro-nav-width:15.5rem]",
        fullHeight ? "h-dvh" : "min-h-full",
      )}
    >
      {/* A hash link would go through the router, so this moves focus itself. */}
      <a
        href="#main"
        className="sr-only focus:not-sr-only focus:fixed focus:top-3 focus:left-3 focus:z-50 focus:rounded-md focus:bg-panel focus:px-3 focus:py-2 focus:text-sm focus:text-fg"
        onClick={(event) => {
          event.preventDefault();
          document.getElementById("main")?.focus();
        }}
      >
        Skip to content
      </a>
      <DesktopSidebar {...shellProps} collapsed={sidebar.collapsed} onToggle={sidebar.toggle} />
      <div className={classNames("flex min-w-0 flex-1 flex-col", fullHeight && "h-dvh")}>
      <MobileNavigation {...shellProps} />

      {!hideHeader && (
        <header
          className={classNames(
            "relative bg-panel after:pointer-events-none after:absolute after:inset-x-0 after:bottom-0 after:border-b after:border-line-strong",
            fullHeight && "shrink-0",
          )}
        >
          <div className={`mx-auto ${maxWidth} px-4 py-4 sm:px-6 lg:px-8`}>
            <div className="flex items-center">
              {!hideTitle && (
                <h1 className="text-xl font-semibold tracking-tight text-fg">
                  {title}
                </h1>
              )}
              {headerExtra && <div className="ml-auto">{headerExtra}</div>}
            </div>
          </div>
        </header>
      )}
      <ShellMain fullHeight={fullHeight} maxWidth={maxWidth} />
      </div>
    </div>
    </TooltipProvider>
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
  return (
    <main
      id="main"
      tabIndex={-1}
      className={classNames(
        "outline-none",
        fullHeight && "min-h-0 flex-1",
      )}
      style={{
        paddingRight: "var(--fabro-ask-sidebar-width, 0px)",
        transition:
          "var(--fabro-ask-sidebar-transition, padding 300ms cubic-bezier(0.16, 1, 0.3, 1))",
      }}
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
