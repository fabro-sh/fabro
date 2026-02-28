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
  ChartBarIcon,
  Cog6ToothIcon,
  PlayCircleIcon,
  RocketLaunchIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { Link, Outlet, useLocation, useMatches } from "react-router";

const user = {
  name: "Tom Cook",
  email: "tom@example.com",
  imageUrl:
    "https://avatars.githubusercontent.com/u/19?v=4",
};

const navigation = [
  { name: "Start", href: "/start", icon: RocketLaunchIcon },
  { name: "Workflow Runs", href: "/runs", icon: PlayCircleIcon },
  { name: "Insights", href: "/insights", icon: ChartBarIcon },
  { name: "Settings", href: "/settings", icon: Cog6ToothIcon },
];

const userNavigation = [{ name: "Sign out", href: "#" }];

function classNames(...classes: Array<string | false | null | undefined>) {
  return classes.filter(Boolean).join(" ");
}

export default function AppShell() {
  const { pathname } = useLocation();
  const matches = useMatches();
  const currentNav = navigation.find((item) => pathname.startsWith(item.href));
  const title = currentNav?.name ?? "";
  const lastMatch = matches[matches.length - 1];
  const headerExtra = (lastMatch?.handle as { headerExtra?: React.ReactNode })
    ?.headerExtra;

  return (
    <div className="min-h-full">
      <Disclosure as="nav" className="bg-navy-800/50">
        <div className="px-4 sm:px-6 lg:px-8">
          <div className="flex h-16 items-center justify-between">
            <div className="flex items-center">
              <div className="shrink-0">
                <img alt="Arc" src="/logotype.svg" className="h-8 w-auto" />
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
                            ? "bg-navy-950/50 text-white"
                            : "text-ice-300 hover:bg-white/5 hover:text-white",
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
              <div className="ml-4 flex items-center md:ml-6">
                <Menu as="div" className="relative">
                  <MenuButton className="relative flex max-w-xs items-center rounded-full focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500">
                    <span className="absolute -inset-1.5" />
                    <span className="sr-only">Open user menu</span>
                    <img
                      alt=""
                      src={user.imageUrl}
                      className="size-8 rounded-full outline -outline-offset-1 outline-white/10"
                    />
                  </MenuButton>

                  <MenuItems
                    transition
                    className="absolute right-0 z-10 mt-2 w-48 origin-top-right rounded-md bg-navy-800 py-1 outline-1 -outline-offset-1 outline-white/10 transition data-closed:scale-95 data-closed:transform data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
                  >
                    {userNavigation.map((item) => (
                      <MenuItem key={item.name}>
                        <a
                          href={item.href}
                          className="block px-4 py-2 text-sm text-ice-300 data-focus:bg-white/5 data-focus:outline-hidden"
                        >
                          {item.name}
                        </a>
                      </MenuItem>
                    ))}
                  </MenuItems>
                </Menu>
              </div>
            </div>
            <div className="-mr-2 flex md:hidden">
              <DisclosureButton className="group relative inline-flex items-center justify-center rounded-md p-2 text-navy-600 hover:bg-white/5 hover:text-white focus:outline-2 focus:outline-offset-2 focus:outline-teal-500">
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
                      ? "bg-navy-950 text-white"
                      : "text-ice-300 hover:bg-white/5 hover:text-white",
                    "flex items-center gap-2 rounded-md px-3 py-2 text-base font-medium",
                  )}
                >
                  <item.icon className="size-5" aria-hidden="true" />
                  {item.name}
                </DisclosureButton>
              );
            })}
          </div>
          <div className="border-t border-white/10 pt-4 pb-3">
            <div className="flex items-center px-5">
              <div className="shrink-0">
                <img
                  alt=""
                  src={user.imageUrl}
                  className="size-10 rounded-full outline -outline-offset-1 outline-white/10"
                />
              </div>
              <div className="ml-3">
                <div className="text-base font-medium text-white">
                  {user.name}
                </div>
                <div className="text-sm font-medium text-navy-600">
                  {user.email}
                </div>
              </div>
            </div>
            <div className="mt-3 space-y-1 px-2">
              {userNavigation.map((item) => (
                <DisclosureButton
                  key={item.name}
                  as="a"
                  href={item.href}
                  className="block rounded-md px-3 py-2 text-base font-medium text-navy-600 hover:bg-white/5 hover:text-white"
                >
                  {item.name}
                </DisclosureButton>
              ))}
            </div>
          </div>
        </DisclosurePanel>
      </Disclosure>

      <header className="relative bg-navy-800 after:pointer-events-none after:absolute after:inset-x-0 after:inset-y-0 after:bottom-0 after:border-y after:border-white/10">
        <div className="flex items-center px-4 py-4 sm:px-6 lg:px-8">
          <h1 className="text-lg/6 font-semibold text-white">{title}</h1>
          {headerExtra && <div className="ml-auto">{headerExtra}</div>}
        </div>
      </header>
      <main>
        <div className="px-4 py-6 sm:px-6 lg:px-8">
          <Outlet />
        </div>
      </main>
    </div>
  );
}
