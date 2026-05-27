import { createElement } from "react";
import { type RouteObject, useParams } from "react-router";

import Root, { ErrorBoundary as RootErrorBoundary } from "./root";
import * as RedirectHome from "./routes/redirect-home";
import * as Setup from "./routes/setup";
import * as AuthLogin from "./routes/auth-login";
import * as Start from "./routes/start";
import * as ChatsLayout from "./routes/chats-layout";
import * as ChatsNew from "./routes/chats-new";
import * as ChatsDetail from "./routes/chats-detail";
import * as AskFabro from "./routes/ask-fabro";
import * as Playground from "./routes/playground";
import * as Automations from "./routes/automations";
import * as AutomationsNew from "./routes/automations-new";
import * as AutomationsEdit from "./routes/automations-edit";
import * as AutomationDetail from "./routes/automation-detail";
import * as Runs from "./routes/runs";
import * as RunDetail from "./routes/run-detail";
import * as RunOverview from "./routes/run-overview";
import * as RunStages from "./routes/run-stages";
import * as RunSettings from "./routes/run-settings";
import * as RunSource from "./routes/run-source";
import * as RunLogs from "./routes/run-logs";
import * as RunEvents from "./routes/run-events";
import * as RunArtifacts from "./routes/run-artifacts";
import * as RunChildren from "./routes/run-children";
import * as RunFiles from "./routes/run-files";
import * as RunSandbox from "./routes/run-sandbox";
import * as RunTerminal from "./routes/run-terminal";
import * as RunBilling from "./routes/run-billing";
import * as Insights from "./routes/insights";
import * as InsightsEditor from "./routes/insights-editor";
import * as InsightsNew from "./routes/insights-new";
import * as Settings from "./routes/settings";
import * as SettingsIndex from "./routes/settings-index";
import * as SettingsGeneral from "./routes/settings-general";
import * as SettingsIntegrations from "./routes/settings-integrations";
import * as SettingsModels from "./routes/settings-models";
import * as SettingsSandboxes from "./routes/settings-sandboxes";
import * as SettingsSecrets from "./routes/settings-secrets";
import * as SettingsSecretsNew from "./routes/settings-secrets-new";
import * as SettingsVariables from "./routes/settings-variables";
import * as SettingsVariablesNew from "./routes/settings-variables-new";
import * as SettingsVariablesEdit from "./routes/settings-variables-edit";
import * as SettingsMonitoring from "./routes/settings-monitoring";
import * as SettingsSecurity from "./routes/settings-security";
import * as SettingsStorage from "./routes/settings-storage";
import * as SettingsLiveEvents from "./routes/settings-live-events";
import * as Profile from "./routes/profile";
import * as ProfileOverview from "./routes/profile-overview";
import * as ProfileSessions from "./routes/profile-sessions";
import AppShellModule from "./layouts/app-shell";

type RouteModule = {
  default: React.ComponentType<any>;
  handle?: RouteObject["handle"];
  ErrorBoundary?: React.ComponentType<any>;
};

function withRouteModule(module: RouteModule) {
  return function WrappedRouteComponent() {
    const params = useParams();
    return createElement(module.default, { params });
  };
}

function route(
  path: string,
  module: RouteModule,
  extra: Omit<RouteObject, "path" | "Component" | "index"> = {},
): RouteObject {
  return {
    path,
    handle: module.handle,
    Component: withRouteModule(module),
    ErrorBoundary: module.ErrorBoundary,
    ...extra,
  };
}

function indexRoute(module: RouteModule): RouteObject {
  return {
    index: true,
    handle: module.handle,
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
      route("runs/:id/terminal", RunTerminal),
      {
        Component: withRouteModule({
          default: AppShellModule,
        }),
        children: [
          route("start", Start),
          route("chats", ChatsLayout, {
            children: [
              route("new", ChatsNew),
              route(":chatId", ChatsDetail),
            ],
          }),
          route("ask-fabro", AskFabro),
          route("playground", Playground),
          route("automations", Automations),
          route("automations/new", AutomationsNew),
          route("automations/:id/edit", AutomationsEdit),
          route("automations/:id", AutomationDetail),
          // Backwards-compatible singular automation route used by older links.
          route("automation/:id", AutomationDetail),
          route("runs", Runs),
          route("runs/:id", RunDetail, {
            children: [
              indexRoute(RunOverview),
              route("stages", RunStages),
              route("stages/:stageId", RunStages),
              route("settings", RunSettings),
              route("source", RunSource),
              route("logs", RunLogs),
              route("events", RunEvents),
              route("artifacts", RunArtifacts),
              route("files", RunFiles),
              route("children", RunChildren),
              route("sandbox", RunSandbox),
              route("billing", RunBilling),
            ],
          }),
          route("insights", Insights, {
            children: [
              indexRoute(InsightsEditor),
              route("new", InsightsNew),
            ],
          }),
          route("settings", Settings, {
            children: [
              indexRoute(SettingsIndex),
              route("models", SettingsModels),
              route("integrations", SettingsIntegrations),
              route("sandboxes", SettingsSandboxes),
              route("security", SettingsSecurity),
              route("variables", SettingsVariables),
              route("variables/new", SettingsVariablesNew),
              route("variables/:name/edit", SettingsVariablesEdit),
              route("secrets", SettingsSecrets),
              route("secrets/new", SettingsSecretsNew),
              route("server", SettingsGeneral),
              route("storage", SettingsStorage),
              route("monitoring", SettingsMonitoring),
              route("live-events", SettingsLiveEvents),
            ],
          }),
          route("profile", Profile, {
            children: [
              indexRoute(ProfileOverview),
              route("sessions", ProfileSessions),
            ],
          }),
        ],
      },
    ],
  },
];
