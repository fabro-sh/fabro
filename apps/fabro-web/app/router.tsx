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
import * as SetupComplete from "./routes/setup-complete";
import * as AuthLogin from "./routes/auth-login";
import * as Start from "./routes/start";
import * as SessionDetail from "./routes/session-detail";
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
import * as RunVerification from "./routes/run-verification";
import * as RunUsage from "./routes/run-usage";
import * as RunRetro from "./routes/run-retro";
import * as VerificationCriteria from "./routes/verification-criteria";
import * as VerificationCriterion from "./routes/verification-criterion";
import * as VerificationControls from "./routes/verification-controls";
import * as VerificationControl from "./routes/verification-control";
import * as Retros from "./routes/retros";
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
      route("setup/complete", SetupComplete),
      route("login", AuthLogin),
      {
        loader: appShellLoader,
        Component: withRouteModule({
          default: AppShellModule,
        }),
        children: [
          route("start", Start),
          route("sessions/:sessionId", SessionDetail),
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
              route("verification", RunVerification),
              route("usage", RunUsage),
              route("retro", RunRetro),
            ],
          }),
          route("verification/criteria", VerificationCriteria),
          route("verification/criteria/:id", VerificationCriterion),
          route("verification/controls", VerificationControls),
          route("verification/controls/:id", VerificationControl),
          route("retros", Retros),
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
