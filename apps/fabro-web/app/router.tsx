import { createElement } from "react";
import {
  type RouteObject,
  useActionData,
  useLoaderData,
  useParams,
} from "react-router";

import Root, { ErrorBoundary as RootErrorBoundary } from "./root";
import * as RedirectHome from "./routes/redirect-home";
import * as Setup from "./routes/setup";
import * as AuthLogin from "./routes/auth-login";
import * as Start from "./routes/start";
import * as Workflows from "./routes/workflows";
import * as WorkflowDetail from "./routes/workflow-detail";
import * as WorkflowDefinition from "./routes/workflow-definition";
import * as WorkflowDiagram from "./routes/workflow-diagram";
import * as WorkflowRuns from "./routes/workflow-runs";
import * as Runs from "./routes/runs";
import * as RunDetail from "./routes/run-detail";
import * as RunOverview from "./routes/run-overview";
import * as RunStages from "./routes/run-stages";
import * as RunSettings from "./routes/run-settings";
import * as RunGraph from "./routes/run-graph";
import * as RunFiles from "./routes/run-files";
import * as RunBilling from "./routes/run-billing";
import * as Insights from "./routes/insights";
import * as InsightsEditor from "./routes/insights-editor";
import * as InsightsNew from "./routes/insights-new";
import * as Settings from "./routes/settings";
import AppShellModule from "./layouts/app-shell";
import { loader as appShellLoader } from "./layouts/app-shell";

type RouteModule = {
  default: React.ComponentType<any>;
  loader?: RouteObject["loader"];
  action?: RouteObject["action"];
  ErrorBoundary?: React.ComponentType<any>;
};

function withRouteModule(module: RouteModule) {
  return function WrappedRouteComponent() {
    const loaderData = useLoaderData();
    const actionData = useActionData();
    const params = useParams();
    return createElement(module.default, { loaderData, actionData, params });
  };
}

function route(
  path: string,
  module: RouteModule,
  extra: Omit<RouteObject, "path" | "Component" | "loader" | "action" | "index"> = {},
): RouteObject {
  return {
    path,
    loader: module.loader,
    action: module.action,
    Component: withRouteModule(module),
    ErrorBoundary: module.ErrorBoundary,
    ...extra,
  };
}

function indexRoute(module: RouteModule): RouteObject {
  return {
    index: true,
    loader: module.loader,
    action: module.action,
    Component: withRouteModule(module),
    ErrorBoundary: module.ErrorBoundary,
  };
}

export const routes: RouteObject[] = [
  {
    path: "/",
    Component: Root,
    ErrorBoundary: RootErrorBoundary,
    children: [
      indexRoute(RedirectHome),
      route("setup", Setup),
      route("login", AuthLogin),
      {
        loader: appShellLoader,
        Component: withRouteModule({
          default: AppShellModule,
        }),
        children: [
          route("start", Start),
          route("workflows", Workflows),
          route("workflows/:name", WorkflowDetail, {
            children: [
              indexRoute(WorkflowDefinition),
              route("diagram", WorkflowDiagram),
              route("runs", WorkflowRuns),
            ],
          }),
          route("runs", Runs),
          route("runs/:id", RunDetail, {
            children: [
              indexRoute(RunOverview),
              route("stages/:stageId", RunStages),
              route("settings", RunSettings),
              route("graph", RunGraph),
              route("files", RunFiles),
              route("billing", RunBilling),
            ],
          }),
          route("insights", Insights, {
            children: [
              indexRoute(InsightsEditor),
              route("new", InsightsNew),
            ],
          }),
          route("settings", Settings),
        ],
      },
    ],
  },
];
