import { describe, expect, test } from "bun:test";
import type { RouteObject } from "react-router";

import { routes } from "./router";

function collectPaths(routeObjects: RouteObject[], prefix = ""): string[] {
  return routeObjects.flatMap((route) => {
    const path = route.index
      ? prefix || "/"
      : route.path
        ? route.path.startsWith("/")
          ? route.path
          : `${prefix}/${route.path}`.replace(/\/+/g, "/")
        : prefix;

    const childPaths = route.children ? collectPaths(route.children, path) : [];
    return route.path || route.index ? [path, ...childPaths] : childPaths;
  });
}

describe("browser router", () => {
  test("uses /login for the sign-in page instead of the backend auth namespace", () => {
    const paths = collectPaths(routes);

    expect(paths).toContain("/login");
    expect(paths).not.toContain("/auth/login");
  });
});
