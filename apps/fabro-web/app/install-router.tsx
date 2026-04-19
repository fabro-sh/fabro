import type { RouteObject } from "react-router";

import Root, { ErrorBoundary as RootErrorBoundary } from "./root";
import InstallApp from "./install-app";

export const installRoutes: RouteObject[] = [
  {
    path: "/",
    Component: Root,
    ErrorBoundary: RootErrorBoundary,
    children: [
      {
        path: "*",
        Component: InstallApp,
      },
    ],
  },
];
