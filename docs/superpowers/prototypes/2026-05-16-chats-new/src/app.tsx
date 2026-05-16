import { Link, NavLink, Outlet } from "react-router";
import {
  ChartBarIcon,
  Cog6ToothIcon,
  PlayIcon,
  SparklesIcon,
} from "@heroicons/react/24/outline";

import { AskFabroProvider } from "./lib/ask-fabro-context";
import AskFabroSidebar from "./components/ask-fabro-sidebar";

const NAV = [
  { name: "Automations", to: "/automations", icon: SparklesIcon },
  { name: "Runs", to: "/runs", icon: PlayIcon },
  { name: "Insights", to: "/insights", icon: ChartBarIcon },
  { name: "Sample", to: "/sample", icon: ChartBarIcon },
  { name: "Settings", to: "/settings", icon: Cog6ToothIcon },
];

/**
 * Mock of the Fabro AppShell top nav. The chat surface mounts beneath it via
 * <Outlet />, exactly the way /chats/new would slot into the real AppShell.
 */
export default function App() {
  return (
    <AskFabroProvider>
      <AppLayout />
    </AskFabroProvider>
  );
}

function AppLayout() {
  return (
    <div className="bg-atmosphere flex h-full text-fg">
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="relative z-10 border-b border-line bg-panel/60 backdrop-blur-sm">
          <div className="flex h-14 items-center gap-6 px-6">
            <Link to="/chats/new" className="flex items-center gap-2">
              <span className="inline-block size-2.5 rounded-full bg-teal-500" />
              <span className="text-sm font-semibold tracking-wide text-fg">
                fabro
              </span>
              <span className="text-xs font-medium text-fg-muted">
                prototype
              </span>
            </Link>
            <nav className="flex items-center gap-1">
              {NAV.map((item) => (
                <NavLink
                  key={item.name}
                  to={item.to}
                  className={({ isActive }) =>
                    [
                      "inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm font-medium transition-colors",
                      isActive
                        ? "bg-overlay text-fg"
                        : "text-fg-muted hover:bg-overlay hover:text-fg",
                    ].join(" ")
                  }
                >
                  <item.icon className="size-4" />
                  {item.name}
                </NavLink>
              ))}
            </nav>
            <div className="ml-auto flex items-center gap-3 text-sm text-fg-muted">
              <span className="hidden sm:inline">bryan@brynary.com</span>
              <span className="inline-flex size-7 items-center justify-center rounded-full bg-overlay text-xs font-medium text-fg ring-1 ring-line-strong">
                BH
              </span>
            </div>
          </div>
        </header>
        <main className="min-h-0 flex-1">
          <Outlet />
        </main>
      </div>
      <AskFabroSidebar />
    </div>
  );
}
