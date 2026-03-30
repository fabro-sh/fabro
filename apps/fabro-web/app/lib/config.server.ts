import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { parse } from "smol-toml";

interface AuthConfig {
  provider: "github" | "tailscale" | "insecure_disabled";
  allowed_usernames: string[];
}

interface ApiConfig {
  base_url: string;
  authentication_strategy: "jwt" | "insecure_disabled";
}

interface GitConfig {
  provider: "github";
  app_id: string | null;
  client_id: string | null;
  slug: string | null;
}

interface Features {
  session_sandboxes: boolean;
  retros: boolean;
}

interface WebConfig {
  url: string;
  auth: AuthConfig;
}

interface AppConfig {
  web: WebConfig;
  api: ApiConfig;
  git: GitConfig;
  features: Features;
}

const AUTH_DEFAULTS: AuthConfig = {
  provider: "github",
  allowed_usernames: [],
};

const WEB_DEFAULTS: WebConfig = {
  url: "http://localhost:5173",
  auth: AUTH_DEFAULTS,
};

const API_DEFAULTS: ApiConfig = {
  base_url: "http://localhost:3000/api/v1",
  authentication_strategy: "jwt",
};

const GIT_DEFAULTS: GitConfig = {
  provider: "github",
  app_id: null,
  client_id: null,
  slug: null,
};

const FEATURES_DEFAULTS: Features = {
  session_sandboxes: false,
  retros: false,
};

export const FABRO_CONFIG_PATH = join(homedir(), ".fabro", "server.toml");

function loadAppConfig(): AppConfig {
  const configPath = FABRO_CONFIG_PATH;

  let raw: Record<string, unknown> = {};
  try {
    raw = parse(readFileSync(configPath, "utf-8")) as Record<string, unknown>;
  } catch {
    // File doesn't exist or is unreadable — use defaults
  }

  const rawWeb = (raw.web ?? {}) as Record<string, unknown>;
  const rawWebAuth = (rawWeb.auth ?? {}) as Partial<AuthConfig>;
  const rawApi = (raw.api ?? {}) as Partial<ApiConfig>;
  const rawGit = (raw.git ?? {}) as Partial<GitConfig>;
  const rawFeatures = (raw.features ?? {}) as Partial<Features>;

  const demo = process.env.FABRO_DEMO === "1";

  return {
    web: {
      ...WEB_DEFAULTS,
      url: (rawWeb.url as string) ?? WEB_DEFAULTS.url,
      auth: demo
        ? { provider: "insecure_disabled", allowed_usernames: [] }
        : { ...AUTH_DEFAULTS, ...rawWebAuth },
    },
    api: demo
      ? { ...API_DEFAULTS, ...rawApi, authentication_strategy: "insecure_disabled" }
      : { ...API_DEFAULTS, ...rawApi },
    git: { ...GIT_DEFAULTS, ...rawGit },
    features: { ...FEATURES_DEFAULTS, ...rawFeatures },
  };
}

/** Loaded once at module init; call reloadAppConfig() to pick up changes. */
let appConfig: AppConfig = loadAppConfig();

export function getAppConfig(): AppConfig {
  return appConfig;
}

export function reloadAppConfig(): void {
  appConfig = loadAppConfig();
}
