import { NavLink, Outlet, useNavigate } from "react-router";
import { PencilSquareIcon } from "@heroicons/react/24/outline";

import { ChatsProvider, useChatsState } from "../lib/chats-store";

export const handle = { hideHeader: true, fullHeight: true, wide: true };

export default function ChatsLayout() {
  return (
    <ChatsProvider>
      {/* Cancel AppShell's content padding (px-4 py-6 / sm:px-6 / lg:px-8) so the
       * chat surface bleeds edge-to-edge below the top nav. The height grows
       * by the vertical padding amount to recover the full viewport area. */}
      <div className="fabro-chat relative isolate -mx-4 -my-6 flex h-[calc(100%+3rem)] sm:-mx-6 lg:-mx-8">
        <Sidebar />
        <div className="min-h-0 flex-1">
          <Outlet />
        </div>
      </div>
    </ChatsProvider>
  );
}

function Sidebar() {
  const state = useChatsState();
  const navigate = useNavigate();
  return (
    <aside className="flex w-64 shrink-0 flex-col border-r border-line bg-panel">
      <div className="p-3">
        <button
          type="button"
          onClick={() => navigate("/chats/new")}
          className="flex w-full items-center justify-center gap-2 rounded-lg bg-overlay-strong px-3 py-2 text-sm font-medium text-fg ring-1 ring-line-strong transition-all hover:bg-overlay-strong hover:ring-line-strong focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
        >
          <PencilSquareIcon className="size-4" />
          New chat
        </button>
      </div>
      <nav className="flex-1 overflow-y-auto px-2 pt-2 pb-3">
        {state.order.map((id) => {
          const chat = state.chats[id];
          if (!chat) return null;
          return (
            <NavLink
              key={id}
              to={`/chats/${id}`}
              className={({ isActive }) =>
                [
                  "relative block truncate rounded-md px-3 py-2 text-sm transition-colors",
                  isActive
                    ? "bg-overlay text-fg before:absolute before:inset-y-2 before:left-0 before:w-0.5 before:rounded-full before:bg-teal-500"
                    : "text-fg-3 hover:bg-overlay/60 hover:text-fg",
                ].join(" ")
              }
              title={chat.title}
            >
              {chat.title || "New chat"}
            </NavLink>
          );
        })}
      </nav>
    </aside>
  );
}
