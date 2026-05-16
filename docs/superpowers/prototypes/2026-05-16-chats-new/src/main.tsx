import { createRoot } from "react-dom/client";
import { createBrowserRouter, Navigate, RouterProvider } from "react-router";

import "./index.css";

import App from "./app";
import ChatsLayout from "./routes/chats-layout";
import ChatsNew from "./routes/chats-new";
import ChatsDetail from "./routes/chats-detail";
import Sample from "./routes/sample";

const router = createBrowserRouter([
  {
    path: "/",
    Component: App,
    children: [
      { index: true, Component: () => <Navigate to="/chats/new" replace /> },
      {
        path: "chats",
        Component: ChatsLayout,
        children: [
          { path: "new", Component: ChatsNew },
          { path: ":chatId", Component: ChatsDetail },
        ],
      },
      { path: "sample", Component: Sample },
    ],
  },
]);

createRoot(document.getElementById("root")!).render(
  <RouterProvider router={router} />,
);
