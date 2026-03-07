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
  BeakerIcon,
  ChartBarIcon,
  CheckBadgeIcon,
  Cog6ToothIcon,
  LightBulbIcon,
  MoonIcon,
  PlayIcon,
  RectangleStackIcon,
  SparklesIcon,
  SunIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { Form, Link, Outlet, redirect, useLocation, useMatches } from "react-router";
import { useTheme } from "../lib/theme";
import { getAppConfig } from "../lib/config.server";
import { isDemoMode, demoCookieHeader } from "../lib/demo-mode.server";
import { isGitHubAppConfigured } from "../lib/github.server";
import { requireUser } from "../lib/session.server";
import type { Route } from "./+types/app-shell";

const DEMO_USER = {
  userUrl: "",
  login: "demo",
  name: "Demo User",
  email: "demo@example.com",
  avatarUrl: "https://github.githubassets.com/assets/GitHub-Mark-ea2971cee799.png",
};

export async function loader({ request }: Route.LoaderArgs) {
  const config = getAppConfig();
  const { provider } = config.web.auth;
  const demoMode = isDemoMode(request);
  if (provider === "insecure_disabled") {
    return { user: DEMO_USER, demoMode, feature_flags: config.feature_flags };
  }
  if (provider === "github" && !isGitHubAppConfigured()) {
    throw redirect("/setup");
  }
  const user = await requireUser(request);
  return { user, provider, demoMode, feature_flags: config.feature_flags };
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  if (form.get("intent") === "toggle-demo") {
    const enabled = !isDemoMode(request);
    const referer = request.headers.get("Referer") ?? "/start";
    const url = new URL(referer);
    return redirect(url.pathname + url.search, {
      headers: { "Set-Cookie": demoCookieHeader(enabled) },
    });
  }
}

const navigation = [
  { name: "Start", href: "/start", icon: SparklesIcon },
  { name: "Workflows", href: "/workflows", icon: RectangleStackIcon },
  { name: "Runs", href: "/runs", icon: PlayIcon },
  { name: "Verification", href: "/verification/criteria", icon: CheckBadgeIcon },
  { name: "Retros", href: "/retros", icon: LightBulbIcon },
  { name: "Insights", href: "/insights", icon: ChartBarIcon },
  { name: "Settings", href: "/settings", icon: Cog6ToothIcon },
];

function classNames(...classes: Array<string | false | null | undefined>) {
  return classes.filter(Boolean).join(" ");
}

export default function AppShell({ loaderData }: Route.ComponentProps) {
  const { user, provider, demoMode } = loaderData;
  const { pathname } = useLocation();
  const matches = useMatches();
  const { theme, toggle } = useTheme();
  const currentNav = navigation.find((item) => pathname.startsWith(item.href));
  const title = currentNav?.name ?? "";
  const lastMatch = matches[matches.length - 1];
  const handle = lastMatch?.handle as { headerExtra?: React.ReactNode } | undefined;
  const headerExtra = handle?.headerExtra;
  const hideHeader = matches.some((m) => (m.handle as { hideHeader?: boolean } | undefined)?.hideHeader);
  const wide = matches.some((m) => (m.handle as { wide?: boolean } | undefined)?.wide);
  const maxWidth = wide ? "" : "max-w-5xl";

  const ThemeIcon = theme === "dark" ? SunIcon : MoonIcon;

  return (
    <div className="min-h-full">
      <Disclosure as="nav" className="bg-panel/50">
        <div className="px-4 sm:px-6 lg:px-8">
          <div className="flex h-16 items-center justify-between">
            <div className="flex items-center">
              <div className="shrink-0">
                <Link to="/start">
                  <img alt="Arc" src={theme === "dark" ? "/logotype.svg" : "/logotype-light.svg"} className="h-8 w-auto" />
                </Link>
              </div>
              <div className="hidden md:block">
                <div className="ml-10 flex items-baseline space-x-4">
                  {navigation.map((item) => {
                    const current = pathname.startsWith(item.href);
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
                <Form method="POST" className="flex">
                  <input type="hidden" name="intent" value="toggle-demo" />
                  <button
                    type="submit"
                    className={classNames(
                      "rounded-full p-1.5 transition-colors hover:bg-overlay hover:text-fg",
                      demoMode ? "text-teal-500" : "text-fg-muted",
                    )}
                    title={demoMode ? "Switch to live data" : "Switch to demo data"}
                  >
                    <BeakerIcon className="size-5" aria-hidden="true" />
                    <span className="sr-only">Toggle demo mode</span>
                  </button>
                </Form>
                <button
                  type="button"
                  onClick={toggle}
                  className="rounded-full p-1.5 text-fg-muted transition-colors hover:bg-overlay hover:text-fg"
                  title={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
                >
                  <ThemeIcon className="size-5" aria-hidden="true" />
                  <span className="sr-only">Toggle theme</span>
                </button>
                {provider !== "tailscale" && (
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
                        <Form method="POST" action="/auth/logout">
                          <button
                            type="submit"
                            className="block w-full px-4 py-2 text-left text-sm text-fg-3 data-focus:bg-overlay data-focus:outline-hidden"
                          >
                            Sign out
                          </button>
                        </Form>
                      </MenuItem>
                    </MenuItems>
                  </Menu>
                )}
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
              const current = pathname.startsWith(item.href);
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
              <div className="ml-auto flex items-center gap-2">
                <Form method="POST" className="flex">
                  <input type="hidden" name="intent" value="toggle-demo" />
                  <button
                    type="submit"
                    className={classNames(
                      "rounded-full p-1.5 transition-colors hover:bg-overlay hover:text-fg",
                      demoMode ? "text-teal-500" : "text-fg-muted",
                    )}
                    title={demoMode ? "Switch to live data" : "Switch to demo data"}
                  >
                    <BeakerIcon className="size-5" aria-hidden="true" />
                    <span className="sr-only">Toggle demo mode</span>
                  </button>
                </Form>
                <button
                  type="button"
                  onClick={toggle}
                  className="rounded-full p-1.5 text-fg-muted transition-colors hover:bg-overlay hover:text-fg"
                  title={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
                >
                  <ThemeIcon className="size-5" aria-hidden="true" />
                  <span className="sr-only">Toggle theme</span>
                </button>
              </div>
            </div>
            {provider !== "tailscale" && (
              <div className="mt-3 space-y-1 px-2">
                <Form method="POST" action="/auth/logout">
                  <DisclosureButton
                    as="button"
                    type="submit"
                    className="block w-full rounded-md px-3 py-2 text-left text-base font-medium text-fg-muted hover:bg-overlay hover:text-fg"
                  >
                    Sign out
                  </DisclosureButton>
                </Form>
              </div>
            )}
          </div>
        </DisclosurePanel>
      </Disclosure>

      {!hideHeader && (
        <header className="relative bg-panel after:pointer-events-none after:absolute after:inset-x-0 after:inset-y-0 after:bottom-0 after:border-y after:border-line-strong">
          <div className={`mx-auto ${maxWidth} px-4 py-4 sm:px-6 lg:px-8`}>
            <div className="flex items-center">
              <h1 className="text-lg/6 font-semibold text-fg">{title}</h1>
              {headerExtra && <div className="ml-auto">{headerExtra}</div>}
            </div>
          </div>
        </header>
      )}
      <main>
        <div className={`mx-auto ${maxWidth} px-4 py-6 sm:px-6 lg:px-8`}>
          <Outlet />
        </div>
      </main>
    </div>
  );
}
