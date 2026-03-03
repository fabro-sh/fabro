import {
  type RouteConfig,
  index,
  layout,
  route,
} from "@react-router/dev/routes";

export default [
  index("routes/redirect-home.tsx"),
  route("setup", "routes/setup.tsx"),
  route("setup/callback", "routes/setup-callback.tsx"),
  route("auth/login", "routes/auth-login.tsx"),
  route("auth/callback", "routes/auth-callback.tsx"),
  route("auth/logout", "routes/auth-logout.tsx"),
  layout("layouts/app-shell.tsx", [
    route("start", "routes/start.tsx"),
    route("sessions/:sessionId", "routes/session-detail.tsx"),
    route("workflows", "routes/workflows.tsx"),
    route("workflows/:name", "routes/workflow-detail.tsx", [
      index("routes/workflow-definition.tsx"),
      route("diagram", "routes/workflow-diagram.tsx"),
      route("runs", "routes/workflow-runs.tsx"),
    ]),
    route("runs", "routes/runs.tsx"),
    route("runs/:id", "routes/run-detail.tsx", [
      index("routes/run-overview.tsx"),
      route("stages/:stageId", "routes/run-stages.tsx"),
      route("configuration", "routes/run-configuration.tsx"),
      route("graph", "routes/run-graph.tsx"),
      route("files", "routes/run-files-changed.tsx"),
      route("verifications", "routes/run-verifications.tsx"),
      route("usage", "routes/run-usage.tsx"),
      route("retro", "routes/run-retro.tsx"),
    ]),
    route("verifications", "routes/verifications.tsx"),
    route("verifications/:slug", "routes/verification-detail.tsx"),
    route("retros", "routes/retros.tsx"),
    route("insights", "routes/insights.tsx", [
      index("routes/insights-editor.tsx"),
      route("new", "routes/insights-new.tsx"),
    ]),
    route("settings", "routes/settings.tsx"),
  ]),
] satisfies RouteConfig;
