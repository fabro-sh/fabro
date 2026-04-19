import { startTransition, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { Link, Navigate, useLocation, useNavigate } from "react-router";

import {
  type InstallFinishResponse,
  type InstallGithubAppOwner,
  type InstallLlmProviderInput,
  type InstallSessionResponse,
  buildInstallGithubAppOwner,
  createInstallGithubAppManifest,
  finishInstall,
  getInstallSession,
  persistInstallToken,
  putInstallGithubToken,
  putInstallLlm,
  putInstallServer,
  readStoredInstallToken,
  testInstallGithubToken,
  testInstallLlm,
} from "./install-api";
import { AuthLayout } from "./components/auth-layout";
import { INSTALL_PROVIDERS } from "./install-config";
import { shouldRedirectAfterHealthPoll } from "./install-flow";
import {
  consumeInstallGithubErrorFromUrl,
  consumeInstallTokenFromUrl,
  shouldConsumeInstallGithubErrorForPath,
} from "./mode";

const INSTALL_STEPS = [
  { id: "welcome", label: "Welcome", href: "/install/welcome" },
  { id: "llm", label: "LLM", href: "/install/llm" },
  { id: "server", label: "Server", href: "/install/server" },
  { id: "github", label: "GitHub", href: "/install/github" },
  { id: "review", label: "Review", href: "/install/review" },
] as const;

type StepId = (typeof INSTALL_STEPS)[number]["id"];
type FinishState = InstallFinishResponse | null;
type GithubStrategy = "token" | "app";
type GithubOwnerKind = "personal" | "org";

type ProviderSelection = Record<
  string,
  {
    apiKey: string;
  }
>;

export default function InstallApp() {
  const navigate = useNavigate();
  const location = useLocation();
  const [installToken, setInstallToken] = useState<string | null>(() =>
    readStoredInstallToken(),
  );
  const [session, setSession] = useState<InstallSessionResponse | null>(null);
  const [loadingSession, setLoadingSession] = useState(false);
  const [sessionError, setSessionError] = useState<string | null>(null);
  const [manualToken, setManualToken] = useState("");
  const [llmSelection, setLlmSelection] = useState<ProviderSelection>(() =>
    defaultProviderSelection(),
  );
  const [canonicalUrl, setCanonicalUrl] = useState("");
  const [githubStrategy, setGithubStrategy] = useState<GithubStrategy>("token");
  const [githubToken, setGithubToken] = useState("");
  const [githubUsername, setGithubUsername] = useState("");
  const [githubOwnerKind, setGithubOwnerKind] =
    useState<GithubOwnerKind>("personal");
  const [githubOrganization, setGithubOrganization] = useState("");
  const [githubAppName, setGithubAppName] = useState("Fabro");
  const [githubAllowedUsername, setGithubAllowedUsername] = useState("");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [finishState, setFinishState] = useState<FinishState>(null);
  const [timedOut, setTimedOut] = useState(false);

  useEffect(() => {
    const { token, sanitizedUrl } = consumeInstallTokenFromUrl(window.location.href);
    if (!token) return;

    persistInstallToken(token);
    setInstallToken(token);
    window.history.replaceState(window.history.state, "", sanitizedUrl);
  }, []);

  useEffect(() => {
    if (shouldConsumeInstallGithubErrorForPath(location.pathname)) {
      const { error, sanitizedUrl } = consumeInstallGithubErrorFromUrl(window.location.href);
      if (error) {
        setSaveError(error);
        window.history.replaceState(window.history.state, "", sanitizedUrl);
        return;
      }
    }
    setSaveError((current) => (current === null ? current : null));
  }, [location.pathname]);

  useEffect(() => {
    if (!installToken) {
      setSession(null);
      return;
    }

    let cancelled = false;
    setLoadingSession(true);
    setSessionError(null);
    getInstallSession(installToken)
      .then((nextSession) => {
        if (cancelled) return;
        setSession(nextSession);
        setCanonicalUrl((current) =>
          current || nextSession.server?.canonical_url || nextSession.prefill.canonical_url,
        );
        setLlmSelection((current) =>
          hydrateProviderSelection(current, nextSession),
        );
        if (nextSession.github?.strategy === "app") {
          setGithubStrategy("app");
          const owner = nextSession.github.owner ?? { kind: "personal" };
          if (owner.kind === "org") {
            setGithubOwnerKind("org");
            setGithubOrganization(owner.slug);
          } else {
            setGithubOwnerKind("personal");
            setGithubOrganization("");
          }
          setGithubAppName(nextSession.github.app_name || "Fabro");
          setGithubAllowedUsername(nextSession.github.allowed_username || "");
        } else if (nextSession.github?.strategy === "token") {
          setGithubStrategy("token");
          setGithubUsername(nextSession.github.username || "");
        }
      })
      .catch((error) => {
        if (cancelled) return;
        setSession(null);
        setSessionError(error instanceof Error ? error.message : "Install session failed");
      })
      .finally(() => {
        if (!cancelled) setLoadingSession(false);
      });

    return () => {
      cancelled = true;
    };
  }, [installToken]);

  useEffect(() => {
    if (!installToken || !session) return;
    if ((location.pathname === "/" || location.pathname === "/install") && !finishState) {
      startTransition(() => {
        navigate("/install/welcome", { replace: true });
      });
    }
  }, [finishState, installToken, location.pathname, navigate, session]);

  useEffect(() => {
    if (!finishState) return;

    setTimedOut(false);
    const deadline = window.setTimeout(() => {
      setTimedOut(true);
    }, 30_000);

    const controller = new AbortController();
    let inFlight = false;
    const poll = async () => {
      if (inFlight || controller.signal.aborted) return;
      inFlight = true;
      try {
        const response = await fetch("/health", { signal: controller.signal });
        const body = response.ok
          ? ((await response.json()) as { mode?: string })
          : undefined;
        if (
          shouldRedirectAfterHealthPoll({
            kind: "response",
            ok: response.ok,
            mode: body?.mode,
          })
        ) {
          window.location.href = finishState.restart_url;
        }
      } catch {
        if (controller.signal.aborted) return;
        if (shouldRedirectAfterHealthPoll({ kind: "error" })) {
          window.location.href = finishState.restart_url;
        }
      } finally {
        inFlight = false;
      }
    };
    const interval = window.setInterval(poll, 2_000);

    return () => {
      controller.abort();
      window.clearTimeout(deadline);
      window.clearInterval(interval);
    };
  }, [finishState]);

  const currentStep = useMemo<StepId>(() => {
    if (location.pathname.startsWith("/install/llm")) return "llm";
    if (location.pathname.startsWith("/install/server")) return "server";
    if (location.pathname.startsWith("/install/github")) return "github";
    if (location.pathname.startsWith("/install/review")) return "review";
    return "welcome";
  }, [location.pathname]);

  const completedSteps = new Set(session?.completed_steps ?? []);

  if (!installToken) {
    return (
      <TokenEntryScreen
        manualToken={manualToken}
        setManualToken={setManualToken}
        sessionError={sessionError}
        onSubmit={() => {
          const nextToken = manualToken.trim();
          if (!nextToken) {
            setSessionError("Paste the install token from the server logs.");
            return;
          }
          persistInstallToken(nextToken);
          setInstallToken(nextToken);
          setSessionError(null);
        }}
      />
    );
  }

  if (loadingSession && !session) {
    return (
      <InstallLayout currentStep={currentStep} completedSteps={completedSteps}>
        <LoadingPanel title="Connecting to install session">
          Reading the current install state from the server.
        </LoadingPanel>
      </InstallLayout>
    );
  }

  if (sessionError && !session) {
    return (
      <TokenEntryScreen
        manualToken={manualToken}
        setManualToken={setManualToken}
        sessionError={sessionError}
        onSubmit={() => {
          const nextToken = manualToken.trim();
          persistInstallToken(nextToken);
          setInstallToken(nextToken || null);
        }}
      />
    );
  }

  if (finishState && location.pathname !== "/install/finishing") {
    return <Navigate to="/install/finishing" replace />;
  }

  return (
    <InstallLayout currentStep={currentStep} completedSteps={completedSteps}>
      {location.pathname === "/install/finishing" ? (
        <FinishingScreen finishState={finishState} timedOut={timedOut} />
      ) : location.pathname === "/install/llm" ? (
        <StepPanel
          eyebrow="LLM providers"
          title="Choose the API keys this server should use."
          description="Each configured provider is validated before the wizard records it. Leave any provider blank to skip it for now."
          error={saveError}
          submitting={submitting}
          onSubmit={async () => {
            const providers = INSTALL_PROVIDERS.map(({ id }) => {
              const current = llmSelection[id] ?? { apiKey: "" };
              return {
                provider: id,
                api_key: current.apiKey.trim(),
              };
            }).filter((provider) => provider.api_key.length > 0);

            if (providers.length === 0) {
              setSaveError("Add at least one provider API key before continuing.");
              return;
            }

            setSubmitting(true);
            setSaveError(null);
            try {
              await Promise.all(
                providers.map((provider) => testInstallLlm(installToken, provider)),
              );
              await putInstallLlm(installToken, providers);
              const nextSession = await getInstallSession(installToken);
              setSession(nextSession);
              navigate("/install/server");
            } catch (error) {
              setSaveError(
                error instanceof Error ? error.message : "Failed to save LLM settings.",
              );
            } finally {
              setSubmitting(false);
            }
          }}
        >
          <ProviderFields value={llmSelection} onChange={setLlmSelection} />
        </StepPanel>
      ) : location.pathname === "/install/server" ? (
        <StepPanel
          eyebrow="Server URL"
          title="Confirm the public URL operators will use."
          description="The install flow uses this URL for redirects, the GitHub App callback, and the final handoff once setup completes."
          error={saveError}
          submitting={submitting}
          onSubmit={async () => {
            if (!canonicalUrl.trim()) {
              setSaveError("Enter the canonical server URL before continuing.");
              return;
            }
            setSubmitting(true);
            setSaveError(null);
            try {
              await putInstallServer(installToken, canonicalUrl.trim());
              const nextSession = await getInstallSession(installToken);
              setSession(nextSession);
              navigate("/install/github");
            } catch (error) {
              setSaveError(
                error instanceof Error ? error.message : "Failed to save server settings.",
              );
            } finally {
              setSubmitting(false);
            }
          }}
        >
          <Field
            label="Canonical URL"
            hint="Detected from forwarded headers when available."
          >
            <input
              value={canonicalUrl}
              onChange={(event) => setCanonicalUrl(event.target.value)}
              className={INPUT_CLASS}
              placeholder="https://fabro.example.com"
            />
          </Field>
        </StepPanel>
      ) : location.pathname === "/install/github/done" ? (
        <GithubAppDoneScreen github={session?.github} />
      ) : location.pathname === "/install/github" ? (
        <StepPanel
          eyebrow="GitHub access"
          title="Choose how Fabro should authenticate to GitHub."
          description="Token installs validate a personal access token and store it in the vault. App installs hand off to GitHub to create an App, then return here automatically."
          error={saveError}
          submitting={submitting}
          submitLabel={githubStrategy === "app" ? "Continue to GitHub" : "Continue"}
          onSubmit={async () => {
            setSubmitting(true);
            setSaveError(null);
            try {
              if (githubStrategy === "token") {
                if (!githubToken.trim()) {
                  setSaveError("Provide the GitHub token before continuing.");
                  return;
                }
                const username = await testInstallGithubToken(
                  installToken,
                  githubToken.trim(),
                );
                setGithubUsername(username);
                await putInstallGithubToken(installToken, githubToken.trim(), username);
                const nextSession = await getInstallSession(installToken);
                setSession(nextSession);
                navigate("/install/review");
                return;
              }

              if (githubOwnerKind === "org" && !githubOrganization.trim()) {
                setSaveError("Enter the organization slug for the GitHub App.");
                return;
              }
              if (!githubAppName.trim()) {
                setSaveError("Enter the GitHub App name before continuing.");
                return;
              }
              if (!githubAllowedUsername.trim()) {
                setSaveError("Enter the GitHub username that should be allowed to log in.");
                return;
              }

              const manifest = await createInstallGithubAppManifest(installToken, {
                owner: buildInstallGithubAppOwner(githubOwnerKind, githubOrganization),
                app_name: githubAppName.trim(),
                allowed_username: githubAllowedUsername.trim(),
              });
              submitGithubManifest(manifest.github_form_action, manifest.manifest);
            } catch (error) {
              setSaveError(
                error instanceof Error ? error.message : "Failed to start GitHub setup.",
              );
            } finally {
              setSubmitting(false);
            }
          }}
        >
          <GithubStrategyPicker
            strategy={githubStrategy}
            onChange={setGithubStrategy}
          />
          {githubStrategy === "token" ? (
            <>
              <Field label="GitHub token">
                <textarea
                  value={githubToken}
                  onChange={(event) => setGithubToken(event.target.value)}
                  className={`${INPUT_CLASS} min-h-28 resize-y`}
                  placeholder="ghp_..."
                />
              </Field>
              <Field
                label="Validated username"
                hint="Filled automatically after token validation."
              >
                <input
                  value={githubUsername}
                  readOnly
                  className={`${INPUT_CLASS} text-fg-3`}
                  placeholder="octocat"
                />
              </Field>
            </>
          ) : (
            <div className="space-y-5">
              <OwnerPicker
                ownerKind={githubOwnerKind}
                setOwnerKind={setGithubOwnerKind}
              />
              {githubOwnerKind === "org" ? (
                <Field label="Organization slug">
                  <input
                    value={githubOrganization}
                    onChange={(event) => setGithubOrganization(event.target.value)}
                    className={INPUT_CLASS}
                    placeholder="acme"
                  />
                </Field>
              ) : null}
              <Field label="GitHub App name">
                <input
                  value={githubAppName}
                  onChange={(event) => setGithubAppName(event.target.value)}
                  className={INPUT_CLASS}
                  placeholder="Fabro"
                />
              </Field>
              <Field
                label="Allowed GitHub username"
                hint="This username is allowed through the runtime GitHub login flow."
              >
                <input
                  value={githubAllowedUsername}
                  onChange={(event) =>
                    setGithubAllowedUsername(event.target.value)
                  }
                  className={INPUT_CLASS}
                  placeholder="octocat"
                />
              </Field>
              {session?.server?.canonical_url ? (
                <div className="rounded-xl border border-line bg-overlay/70 px-4 py-4 text-sm text-fg-3">
                  The GitHub App will redirect back to{" "}
                  <code>{session.server.canonical_url}</code>.
                </div>
              ) : null}
            </div>
          )}
        </StepPanel>
      ) : location.pathname === "/install/review" ? (
        <ReviewScreen
          session={session}
          error={saveError}
          submitting={submitting}
          onInstall={async () => {
            setSubmitting(true);
            setSaveError(null);
            try {
              const result = await finishInstall(installToken);
              setFinishState(result);
              navigate("/install/finishing");
            } catch (error) {
              setSaveError(error instanceof Error ? error.message : "Install failed.");
            } finally {
              setSubmitting(false);
            }
          }}
        />
      ) : (
        <WelcomeScreen />
      )}
    </InstallLayout>
  );
}

function TokenEntryScreen({
  manualToken,
  setManualToken,
  sessionError,
  onSubmit,
}: {
  manualToken: string;
  setManualToken: (value: string) => void;
  sessionError: string | null;
  onSubmit: () => void;
}) {
  return (
    <AuthLayout footer="Fabro install mode is temporary and only available until setup completes.">
      <p className="text-center text-xs font-medium uppercase tracking-[0.24em] text-teal-300">
        Install Mode
      </p>
      <h1 className="mt-3 text-center text-2xl font-semibold tracking-tight text-fg">
        Finish configuring this Fabro server.
      </h1>
      <p className="mt-3 text-center text-sm leading-relaxed text-fg-3">
        Find the one-time install token in your terminal output, Docker logs, or
        platform logs, then paste it here to continue.
      </p>
      <div className="mt-6 space-y-4">
        <textarea
          value={manualToken}
          onChange={(event) => setManualToken(event.target.value)}
          className={`${INPUT_CLASS} min-h-28 resize-y`}
          placeholder="Paste install token"
        />
        {sessionError ? (
          <div className="rounded-lg border border-coral/40 bg-coral/10 px-4 py-3 text-sm text-fg-2">
            {sessionError}
          </div>
        ) : null}
        <button
          type="button"
          onClick={onSubmit}
          className={PRIMARY_BUTTON_CLASS}
        >
          Continue
        </button>
        <div className="rounded-lg border border-line-strong bg-overlay px-4 py-3 text-sm text-fg-3">
          <p className="font-medium text-fg-2">Where to look</p>
          <p className="mt-2">Local terminal: the `fabro server start` output.</p>
          <p className="mt-1">Docker: `docker logs &lt;container&gt;`.</p>
          <p className="mt-1">Railway/systemd: your platform log viewer or `journalctl`.</p>
        </div>
      </div>
    </AuthLayout>
  );
}

function InstallLayout({
  children,
  currentStep,
  completedSteps,
}: {
  children: ReactNode;
  currentStep: StepId;
  completedSteps: Set<string>;
}) {
  return (
    <main className="min-h-screen bg-atmosphere px-4 py-8 text-fg">
      <div className="mx-auto grid max-w-6xl gap-6 lg:grid-cols-[240px_minmax(0,1fr)]">
        <aside className="rounded-2xl border border-line bg-panel/70 p-5 backdrop-blur-sm">
          <div className="flex items-center gap-3">
            <img src="/logo.svg" alt="Fabro" className="h-10 w-10" />
            <div>
              <p className="text-xs font-medium uppercase tracking-[0.24em] text-teal-300">
                Web Install
              </p>
              <h2 className="mt-1 text-lg font-semibold tracking-tight text-fg">
                Server setup
              </h2>
            </div>
          </div>
          <ol className="mt-6 space-y-3">
            {INSTALL_STEPS.map((step, index) => {
              const active = step.id === currentStep;
              const complete = completedSteps.has(step.id);
              const linkable = active || complete || step.id === "welcome";
              const inner = (
                <>
                  <span
                    className={`flex h-8 w-8 items-center justify-center rounded-full border text-xs font-semibold ${
                      complete
                        ? "border-mint/50 bg-mint/15 text-mint"
                        : active
                          ? "border-teal-300/60 bg-teal-300/10 text-teal-300"
                          : "border-line-strong text-fg-muted"
                    }`}
                  >
                    {complete ? "✓" : index + 1}
                  </span>
                  <span className="text-sm font-medium">{step.label}</span>
                </>
              );

              return (
                <li key={step.id}>
                  {linkable ? (
                    <Link
                      to={step.href}
                      className={`flex items-center gap-3 rounded-xl border px-3 py-3 transition-colors ${
                        active
                          ? "border-teal-500/40 bg-teal-500/12 text-fg"
                          : "border-line bg-overlay/60 text-fg-3 hover:border-line-strong hover:text-fg"
                      }`}
                    >
                      {inner}
                    </Link>
                  ) : (
                    <span className="flex items-center gap-3 rounded-xl border border-line bg-overlay/30 px-3 py-3 text-fg-muted">
                      {inner}
                    </span>
                  )}
                </li>
              );
            })}
          </ol>
        </aside>
        <section className="rounded-2xl border border-line bg-panel/85 p-6 shadow-lg backdrop-blur-sm lg:p-8">
          {children}
        </section>
      </div>
    </main>
  );
}

function WelcomeScreen() {
  return (
    <div className="space-y-6">
      <header>
        <p className="text-xs font-medium uppercase tracking-[0.24em] text-teal-300">
          Welcome
        </p>
        <h1 className="mt-3 text-3xl font-semibold tracking-tight text-fg">
          This wizard writes the same local state as `fabro install`.
        </h1>
        <p className="mt-3 max-w-2xl text-sm leading-relaxed text-fg-3">
          You&apos;ll validate LLM credentials, confirm the public server URL,
          choose a GitHub token or GitHub App, review the generated state, and
          then let the server restart into normal mode.
        </p>
      </header>
      <div className="grid gap-4 md:grid-cols-3">
        {[
          ["LLM", "Validate the provider credentials this server should use."],
          ["Server URL", "Confirm the URL operators will use after setup completes."],
          ["GitHub", "Choose either a stored token or a GitHub App install flow."],
        ].map(([title, body]) => (
          <article
            key={title}
            className="rounded-xl border border-line bg-overlay/70 p-4"
          >
            <h2 className="text-sm font-semibold text-fg">{title}</h2>
            <p className="mt-2 text-sm leading-relaxed text-fg-3">{body}</p>
          </article>
        ))}
      </div>
      <div className="flex justify-end">
        <Link to="/install/llm" className={PRIMARY_BUTTON_CLASS}>
          Start setup
        </Link>
      </div>
    </div>
  );
}

function StepPanel({
  eyebrow,
  title,
  description,
  children,
  error,
  submitting,
  submitLabel = "Continue",
  onSubmit,
}: {
  eyebrow: string;
  title: string;
  description: string;
  children: ReactNode;
  error: string | null;
  submitting: boolean;
  submitLabel?: string;
  onSubmit: () => Promise<void>;
}) {
  return (
    <div className="space-y-6">
      <header>
        <p className="text-xs font-medium uppercase tracking-[0.24em] text-teal-300">
          {eyebrow}
        </p>
        <h1 className="mt-3 text-3xl font-semibold tracking-tight text-fg">
          {title}
        </h1>
        <p className="mt-3 max-w-2xl text-sm leading-relaxed text-fg-3">
          {description}
        </p>
      </header>
      <div className="space-y-5">{children}</div>
      {error ? (
        <div className="rounded-xl border border-coral/40 bg-coral/10 px-4 py-3 text-sm text-fg-2">
          {error}
        </div>
      ) : null}
      <div className="flex justify-end">
        <button
          type="button"
          disabled={submitting}
          onClick={() => void onSubmit()}
          className={PRIMARY_BUTTON_CLASS}
        >
          {submitting ? "Saving…" : submitLabel}
        </button>
      </div>
    </div>
  );
}

function ReviewScreen({
  session,
  error,
  submitting,
  onInstall,
}: {
  session: InstallSessionResponse | null;
  error: string | null;
  submitting: boolean;
  onInstall: () => Promise<void>;
}) {
  return (
    <div className="space-y-6">
      <header>
        <p className="text-xs font-medium uppercase tracking-[0.24em] text-teal-300">
          Review
        </p>
        <h1 className="mt-3 text-3xl font-semibold tracking-tight text-fg">
          Confirm the install plan before writing files to disk.
        </h1>
      </header>
      <div className="grid gap-4 md:grid-cols-3">
        <SummaryCard
          title="LLM"
          body={
            (session?.llm?.providers ?? []).map((provider) => provider.provider).join(", ") ||
            "Not configured"
          }
        />
        <SummaryCard
          title="Server URL"
          body={session?.server?.canonical_url || session?.prefill.canonical_url || "Unknown"}
        />
        <SummaryCard
          title="GitHub"
          body={describeGithubSummary(session?.github)}
        />
      </div>
      {error ? (
        <div className="rounded-xl border border-coral/40 bg-coral/10 px-4 py-3 text-sm text-fg-2">
          {error}
        </div>
      ) : null}
      <div className="flex justify-end">
        <button
          type="button"
          disabled={submitting}
          onClick={() => void onInstall()}
          className={PRIMARY_BUTTON_CLASS}
        >
          {submitting ? "Installing…" : "Install"}
        </button>
      </div>
    </div>
  );
}

function FinishingScreen({
  finishState,
  timedOut,
}: {
  finishState: FinishState;
  timedOut: boolean;
}) {
  if (!finishState) {
    return <Navigate to="/install/review" replace />;
  }

  return (
    <div className="space-y-6">
      <header>
        <p className="text-xs font-medium uppercase tracking-[0.24em] text-teal-300">
          Finishing
        </p>
        <h1 className="mt-3 text-3xl font-semibold tracking-tight text-fg">
          Install complete. Waiting for the configured server to come back up.
        </h1>
        <p className="mt-3 max-w-2xl text-sm leading-relaxed text-fg-3">
          If this deployment is supervised, the process should restart automatically.
          If not, this screen will switch to the manual next step shortly.
        </p>
      </header>
      <div className="rounded-xl border border-line bg-overlay/70 p-5">
        <p className="text-xs font-medium uppercase tracking-[0.24em] text-fg-muted">
          Development token
        </p>
        <pre className="mt-3 overflow-x-auto rounded-lg border border-line-strong bg-panel-alt px-4 py-3 font-mono text-sm text-fg-2">
          <code>{finishState.dev_token}</code>
        </pre>
      </div>
      {timedOut ? (
        <div className="rounded-xl border border-amber/40 bg-amber/10 px-5 py-4 text-sm text-fg-2">
          <p className="font-medium text-fg">Install complete</p>
          <p className="mt-2">
            Run <code>fabro server start</code> to launch the configured server,
            then return to <code>{finishState.restart_url}</code>.
          </p>
        </div>
      ) : (
        <LoadingPanel title="Waiting for restart">
          Polling <code>/health</code> until the server leaves install mode.
        </LoadingPanel>
      )}
    </div>
  );
}

function ProviderFields({
  value,
  onChange,
}: {
  value: ProviderSelection;
  onChange: (nextValue: ProviderSelection) => void;
}) {
  return (
    <div className="space-y-4">
      {INSTALL_PROVIDERS.map((provider) => {
        const current = value[provider.id] ?? { apiKey: "" };
        return (
          <div
            key={provider.id}
            className="rounded-xl border border-line bg-overlay/70 p-4"
          >
            <Field label={provider.label} hint={provider.hint}>
              <input
                value={current.apiKey}
                onChange={(event) =>
                  onChange({
                    ...value,
                    [provider.id]: {
                      ...current,
                      apiKey: event.target.value,
                    },
                  })}
                className={INPUT_CLASS}
                placeholder={`${provider.label} API key`}
              />
            </Field>
          </div>
        );
      })}
    </div>
  );
}

function GithubStrategyPicker({
  strategy,
  onChange,
}: {
  strategy: GithubStrategy;
  onChange: (value: GithubStrategy) => void;
}) {
  return (
    <div className="grid gap-4 md:grid-cols-2">
      {[
        {
          id: "token",
          title: "Personal access token",
          body: "Validate a PAT, store it in the vault, and move straight to review.",
        },
        {
          id: "app",
          title: "GitHub App",
          body: "Create an App on GitHub, return through the browser callback, then finish setup here.",
        },
      ].map((option) => (
        <button
          key={option.id}
          type="button"
          onClick={() => onChange(option.id as GithubStrategy)}
          className={`rounded-xl border px-4 py-4 text-left transition-colors ${
            strategy === option.id
              ? "border-teal-500/40 bg-teal-500/12"
              : "border-line bg-overlay/60 hover:border-line-strong"
          }`}
        >
          <p className="text-sm font-semibold text-fg">{option.title}</p>
          <p className="mt-2 text-sm leading-relaxed text-fg-3">{option.body}</p>
        </button>
      ))}
    </div>
  );
}

function OwnerPicker({
  ownerKind,
  setOwnerKind,
}: {
  ownerKind: GithubOwnerKind;
  setOwnerKind: (value: GithubOwnerKind) => void;
}) {
  return (
    <div className="grid gap-4 md:grid-cols-2">
      {[
        {
          id: "personal",
          title: "Personal account",
          body: "Use GitHub’s personal app creation flow.",
        },
        {
          id: "org",
          title: "Organization",
          body: "Use the organization app creation flow and specify the org slug.",
        },
      ].map((option) => (
        <button
          key={option.id}
          type="button"
          onClick={() => setOwnerKind(option.id as GithubOwnerKind)}
          className={`rounded-xl border px-4 py-4 text-left transition-colors ${
            ownerKind === option.id
              ? "border-teal-500/40 bg-teal-500/12"
              : "border-line bg-overlay/60 hover:border-line-strong"
          }`}
        >
          <p className="text-sm font-semibold text-fg">{option.title}</p>
          <p className="mt-2 text-sm leading-relaxed text-fg-3">{option.body}</p>
        </button>
      ))}
    </div>
  );
}

function GithubAppDoneScreen({
  github,
}: {
  github: InstallSessionResponse["github"];
}) {
  if (!github || github.strategy !== "app") {
    return <Navigate to="/install/github" replace />;
  }

  return (
    <div className="space-y-6">
      <header>
        <p className="text-xs font-medium uppercase tracking-[0.24em] text-teal-300">
          GitHub App Ready
        </p>
        <h1 className="mt-3 text-3xl font-semibold tracking-tight text-fg">
          GitHub returned the App credentials to this install session.
        </h1>
        <p className="mt-3 max-w-2xl text-sm leading-relaxed text-fg-3">
          The App is connected and ready to be written into the runtime env file
          during the final install step.
        </p>
      </header>
      <div className="grid gap-4 md:grid-cols-3">
        <SummaryCard title="Owner" body={describeGithubAppOwner(github.owner)} />
        <SummaryCard title="App" body={github.slug || github.app_name || "GitHub App"} />
        <SummaryCard
          title="Allowed user"
          body={github.allowed_username || "Unknown"}
        />
      </div>
      <div className="flex justify-end">
        <Link to="/install/review" className={PRIMARY_BUTTON_CLASS}>
          Continue to review
        </Link>
      </div>
    </div>
  );
}

function LoadingPanel({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <div className="rounded-xl border border-line bg-overlay/70 px-5 py-4">
      <p className="text-sm font-semibold text-fg">{title}</p>
      <p className="mt-2 text-sm text-fg-3">{children}</p>
    </div>
  );
}

function SummaryCard({ title, body }: { title: string; body: string }) {
  return (
    <article className="rounded-xl border border-line bg-overlay/70 p-4">
      <p className="text-xs font-medium uppercase tracking-[0.24em] text-fg-muted">
        {title}
      </p>
      <p className="mt-3 text-sm leading-relaxed text-fg-2">{body}</p>
    </article>
  );
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label className="block space-y-2">
      <div className="flex items-center justify-between gap-4">
        <span className="text-sm font-medium text-fg">{label}</span>
        {hint ? <span className="text-xs text-fg-muted">{hint}</span> : null}
      </div>
      {children}
    </label>
  );
}

function defaultProviderSelection(): ProviderSelection {
  return Object.fromEntries(
    INSTALL_PROVIDERS.map((provider) => [
      provider.id,
      {
        apiKey: "",
      },
    ]),
  );
}

function hydrateProviderSelection(
  current: ProviderSelection,
  session: InstallSessionResponse,
): ProviderSelection {
  const hasUserInput = Object.values(current).some((provider) => provider.apiKey);
  if (hasUserInput) return current;

  const next = defaultProviderSelection();
  for (const provider of session.llm?.providers ?? []) {
    next[provider.provider] = {
      apiKey: "",
    };
  }
  return next;
}

function describeGithubSummary(github: InstallSessionResponse["github"]): string {
  if (!github) return "Not configured";
  if (github.strategy === "app") {
    const appLabel = github.slug || github.app_name || "GitHub App";
    const userLabel = github.allowed_username
      ? `allowed ${github.allowed_username}`
      : "allowed user unset";
    return `${appLabel} · ${userLabel}`;
  }
  return github.username ? `Token for ${github.username}` : "Token configured";
}

function describeGithubAppOwner(
  owner: InstallGithubAppOwner | undefined,
): string {
  if (!owner || owner.kind === "personal") return "personal";
  return owner.slug ? `org:${owner.slug}` : "org";
}

function submitGithubManifest(
  formAction: string,
  manifest: Record<string, unknown>,
): void {
  const form = document.createElement("form");
  form.method = "post";
  form.action = formAction;
  form.style.display = "none";

  const input = document.createElement("input");
  input.type = "hidden";
  input.name = "manifest";
  input.value = JSON.stringify(manifest);

  form.appendChild(input);
  document.body.appendChild(form);
  form.submit();
}

const INPUT_CLASS =
  "w-full rounded-xl border border-line-strong bg-panel-alt px-4 py-3 text-sm text-fg outline-none transition focus:border-teal-300/70 focus:ring-4 focus:ring-teal-500/20";

const PRIMARY_BUTTON_CLASS =
  "inline-flex items-center justify-center rounded-xl bg-teal-500 px-4 py-2.5 text-sm font-medium text-white transition hover:bg-teal-300 disabled:cursor-not-allowed disabled:opacity-70";
