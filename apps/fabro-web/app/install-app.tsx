import { startTransition, useEffect, useMemo, useRef, useState } from "react";
import type { FormEvent, ReactNode, Ref } from "react";
import { Link, Navigate, useLocation, useNavigate } from "react-router";
import {
  ArrowLeftIcon,
  ArrowRightIcon,
  ArrowTopRightOnSquareIcon,
  CheckCircleIcon,
  CheckIcon,
  ChevronDownIcon,
  ClipboardDocumentCheckIcon,
  ClipboardIcon,
  EyeIcon,
  EyeSlashIcon,
} from "@heroicons/react/16/solid";

import {
  type InstallFinishResponse,
  type InstallGithubAppOwner,
  type InstallLlmProviderInput,
  type InstallObjectStoreInput,
  type InstallSandboxInput,
  type InstallSessionResponse,
  createInstallGithubAppManifest,
  finishInstall,
  getInstallSession,
  persistInstallToken,
  putInstallGithubToken,
  putInstallLlm,
  putInstallObjectStore,
  putInstallSandbox,
  putInstallServer,
  readStoredInstallToken,
  testInstallGithubToken,
  testInstallLlm,
  testInstallObjectStore,
  testInstallSandbox,
} from "./install-api";
import { INSTALL_PROVIDERS } from "./install-config";
import { shouldRedirectAfterHealthPoll } from "./install-flow";
import {
  consumeInstallGithubErrorFromUrl,
  consumeInstallTokenFromUrl,
  shouldConsumeInstallGithubErrorForPath,
} from "./mode";
import {
  CopyButton,
  ErrorMessage,
  INPUT_CLASS,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "./components/ui";
import { LoadingState } from "./components/state";

const INSTALL_STEPS = [
  { id: "welcome", label: "Welcome", href: "/install/welcome" },
  { id: "server", label: "Server", href: "/install/server" },
  { id: "object_store", label: "Storage", href: "/install/object-store" },
  { id: "sandbox", label: "Sandbox", href: "/install/sandbox" },
  { id: "llm", label: "LLMs", href: "/install/llm" },
  { id: "github", label: "GitHub", href: "/install/github" },
  { id: "review", label: "Review", href: "/install/review" },
] as const;

const STEPPER_STEPS = INSTALL_STEPS.slice(1);

type StepId = (typeof INSTALL_STEPS)[number]["id"];
type FinishState = InstallFinishResponse | null;
type GithubStrategy = "token" | "app";
type GithubOwnerKind = "personal" | "org";

type SessionState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "ready"; data: InstallSessionResponse };

type TokenForm = { token: string; username: string };

type AppForm = {
  owner: InstallGithubAppOwner;
  appName: string;
  allowedUsername: string;
};

type ProviderSelection = Record<string, { apiKey: string }>;
type ObjectStoreProvider = "local" | "s3";
type ObjectStoreCredentialMode = "runtime" | "access_key";
type ObjectStoreForm = {
  provider: ObjectStoreProvider;
  localRoot: string;
  bucket: string;
  region: string;
  credentialMode: ObjectStoreCredentialMode;
  accessKeyId: string;
  secretAccessKey: string;
  manualCredentialsSaved: boolean;
};
type SandboxProvider = NonNullable<InstallSandboxInput["provider"]>;
type SandboxForm = {
  provider: SandboxProvider;
  apiKey:   string;
  apiKeySaved: boolean;
};

export default function InstallApp() {
  const navigate = useNavigate();
  const location = useLocation();
  const [installToken, setInstallToken] = useState<string | null>(() =>
    readStoredInstallToken(),
  );
  const [sessionState, setSessionState] = useState<SessionState>({ status: "idle" });
  const session = sessionState.status === "ready" ? sessionState.data : null;
  const [manualToken, setManualToken] = useState("");
  const [llmSelection, setLlmSelection] = useState<ProviderSelection>(() =>
    defaultProviderSelection(),
  );
  const [objectStoreForm, setObjectStoreForm] = useState<ObjectStoreForm>(() =>
    defaultObjectStoreForm(),
  );
  const [sandboxForm, setSandboxForm] = useState<SandboxForm>(() =>
    defaultSandboxForm(),
  );
  const [canonicalUrl, setCanonicalUrl] = useState("");
  const [githubStrategy, setGithubStrategy] = useState<GithubStrategy>("token");
  const [tokenForm, setTokenForm] = useState<TokenForm>({ token: "", username: "" });
  const [appForm, setAppForm] = useState<AppForm>({
    owner:           { kind: "personal" },
    appName:         "Fabro",
    allowedUsername: "",
  });
  const [saveError, setSaveError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [finishState, setFinishState] = useState<FinishState>(null);
  const [timedOut, setTimedOut] = useState(false);
  const canonicalUrlInputRef = useRef<HTMLInputElement>(null);
  const localRootInputRef = useRef<HTMLInputElement>(null);
  const bucketInputRef = useRef<HTMLInputElement>(null);
  const regionInputRef = useRef<HTMLInputElement>(null);
  const accessKeyIdInputRef = useRef<HTMLInputElement>(null);
  const secretAccessKeyInputRef = useRef<HTMLInputElement>(null);
  const sandboxApiKeyInputRef = useRef<HTMLInputElement>(null);

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
      setSessionState({ status: "idle" });
      return;
    }

    let cancelled = false;
    setSessionState({ status: "loading" });
    getInstallSession(installToken)
      .then((nextSession) => {
        if (cancelled) return;
        setSessionState({ status: "ready", data: nextSession });
        setCanonicalUrl((current) =>
          current || nextSession.server?.canonical_url || nextSession.prefill.canonical_url,
        );
        setObjectStoreForm(hydrateObjectStoreForm(nextSession));
        setSandboxForm((current) => hydrateSandboxForm(current, nextSession));
        setLlmSelection((current) =>
          hydrateProviderSelection(current, nextSession),
        );
        if (nextSession.github?.strategy === "app") {
          setGithubStrategy("app");
          setAppForm({
            owner:           nextSession.github.owner ?? { kind: "personal" },
            appName:         nextSession.github.app_name || "Fabro",
            allowedUsername: nextSession.github.allowed_username || "",
          });
        } else if (nextSession.github?.strategy === "token") {
          setGithubStrategy("token");
          setTokenForm((current) => ({
            ...current,
            username: nextSession.github?.username || current.username,
          }));
        }
      })
      .catch((error) => {
        if (cancelled) return;
        setSessionState({
          status:  "error",
          message: error instanceof Error ? error.message : "Install session failed",
        });
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

  const currentStep = useMemo<StepId>(
    () =>
      STEPPER_STEPS.find((step) => location.pathname.startsWith(step.href))?.id ??
      "welcome",
    [location.pathname],
  );

  const completedSteps = new Set(session?.completed_steps ?? []);

  const sessionError =
    sessionState.status === "error" ? sessionState.message : null;

  if (!installToken) {
    return (
      <TokenEntryScreen
        manualToken={manualToken}
        setManualToken={setManualToken}
        sessionError={sessionError}
        onSubmit={() => {
          const nextToken = manualToken.trim();
          if (!nextToken) {
            setSessionState({
              status:  "error",
              message: "Paste the install token from the server logs.",
            });
            return;
          }
          persistInstallToken(nextToken);
          setInstallToken(nextToken);
          setSessionState({ status: "idle" });
        }}
      />
    );
  }

  const runStepSubmit = async (args: {
    action:   () => Promise<void>;
    fallback: string;
    next?:    string;
  }) => {
    setSubmitting(true);
    setSaveError(null);
    try {
      await args.action();
      if (args.next) {
        const nextSession = await getInstallSession(installToken);
        setSessionState({ status: "ready", data: nextSession });
        setSandboxForm((current) => hydrateSandboxForm(current, nextSession));
        navigate(args.next);
      }
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : args.fallback);
    } finally {
      setSubmitting(false);
    }
  };

  if (sessionState.status === "error") {
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

  // Covers both sessionState "loading" AND the brief "idle" window between
  // the initial render and the session-fetch useEffect. Without this guard,
  // screens like GithubAppDoneScreen see `session == null` and navigate away
  // before the first fetch finishes — trapping the user in a redirect loop.
  if (!session) {
    return (
      <InstallLayout currentStep={currentStep} completedSteps={completedSteps}>
        <LoadingState label="Connecting to install session…" />
      </InstallLayout>
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
          title="Add your LLM credentials"
          description="Each key you enter is validated before it's saved. Skip a provider by leaving it blank."
          error={saveError}
          submitting={submitting}
          backHref="/install/sandbox"
          onSubmit={async () => {
            const providers = INSTALL_PROVIDERS.map(({ id }) => {
              const current = llmSelection[id] ?? { apiKey: "" };
              return {
                provider: id,
                api_key:  current.apiKey.trim(),
              };
            }).filter((provider) => provider.api_key.length > 0);

            if (providers.length === 0) {
              setSaveError("Add at least one provider API key before continuing.");
              return;
            }

            await runStepSubmit({
              action: async () => {
                await Promise.all(
                  providers.map((provider) => testInstallLlm(installToken, provider)),
                );
                await putInstallLlm(installToken, providers);
              },
              fallback: "Failed to save LLM settings.",
              next:     "/install/github",
            });
          }}
        >
          <ProviderFields value={llmSelection} onChange={setLlmSelection} />
        </StepPanel>
      ) : location.pathname === "/install/server" ? (
        <StepPanel
          title="Confirm the public URL"
          description="This is where operators will reach Fabro after setup. It's also the redirect target for the GitHub App callback."
          error={saveError}
          submitting={submitting}
          backHref="/install/welcome"
          onSubmit={async () => {
            if (!canonicalUrl.trim()) {
              setSaveError("Enter the canonical server URL before continuing.");
              focusInput(canonicalUrlInputRef);
              return;
            }
            await runStepSubmit({
              action:   () => putInstallServer(installToken, canonicalUrl.trim()),
              fallback: "Failed to save server settings.",
              next:     "/install/object-store",
            });
          }}
        >
          <Field
            label="Canonical URL"
            hint="Auto-detected from forwarded headers when available."
          >
            <input
              type="url"
              name="canonical_url"
              ref={canonicalUrlInputRef}
              value={canonicalUrl}
              onChange={(event) => setCanonicalUrl(event.target.value)}
              className={INPUT_CLASS}
              placeholder="https://fabro.example.com"
              autoComplete="url"
              spellCheck={false}
            />
          </Field>
        </StepPanel>
      ) : location.pathname === "/install/object-store" ? (
        <StepPanel
          title="Choose the shared object store"
          description="This configures the shared backend for both SlateDB and run artifacts. Fabro still keeps its local storage root on disk."
          error={saveError}
          submitting={submitting}
          submittingLabel={
            objectStoreForm.provider === "s3" ? "Checking access..." : "Saving..."
          }
          backHref="/install/server"
          onSubmit={async () => {
            if (objectStoreForm.provider === "local") {
              if (!objectStoreForm.localRoot.trim()) {
                setSaveError("Enter the local object-store directory before continuing.");
                focusInput(localRootInputRef);
                return;
              }
            } else {
              if (!objectStoreForm.bucket.trim()) {
                setSaveError("Enter the S3 bucket before continuing.");
                focusInput(bucketInputRef);
                return;
              }
              if (!objectStoreForm.region.trim()) {
                setSaveError("Enter the AWS region before continuing.");
                focusInput(regionInputRef);
                return;
              }
              if (objectStoreForm.credentialMode === "access_key") {
                const accessKeyId = objectStoreForm.accessKeyId.trim();
                const secretAccessKey = objectStoreForm.secretAccessKey.trim();
                const keepStoredCredentials =
                  objectStoreForm.manualCredentialsSaved &&
                  !accessKeyId &&
                  !secretAccessKey;
                if (!keepStoredCredentials && !accessKeyId) {
                  setSaveError("Enter the AWS access key ID before continuing.");
                  focusInput(accessKeyIdInputRef);
                  return;
                }
                if (!keepStoredCredentials && !secretAccessKey) {
                  setSaveError("Enter the AWS secret access key before continuing.");
                  focusInput(secretAccessKeyInputRef);
                  return;
                }
              }
            }

            const payload = buildObjectStorePayload(objectStoreForm);
            await runStepSubmit({
              action: async () => {
                if (objectStoreForm.provider === "s3") {
                  await testInstallObjectStore(installToken, payload);
                }
                await putInstallObjectStore(installToken, payload);
              },
              fallback: "Failed to save object-store settings.",
              next:     "/install/sandbox",
            });
          }}
        >
          <CardPicker
            legend="Object store"
            options={OBJECT_STORE_PROVIDER_OPTIONS}
            value={objectStoreForm.provider}
            onChange={(provider) => {
              setObjectStoreForm((current) => ({ ...current, provider }));
              if (provider === "s3") {
                focusInput(bucketInputRef);
              } else {
                focusInput(localRootInputRef);
              }
            }}
          />
          {objectStoreForm.provider === "s3" ? (
            <div className="space-y-5">
              <Field label="Bucket">
                <input
                  ref={bucketInputRef}
                  name="object_store_bucket"
                  value={objectStoreForm.bucket}
                  onChange={(event) =>
                    setObjectStoreForm((current) => ({
                      ...current,
                      bucket: event.target.value,
                    }))
                  }
                  className={`${INPUT_CLASS} font-mono`}
                  placeholder="my-fabro-data"
                  spellCheck={false}
                  autoCapitalize="off"
                />
              </Field>
              <Field label="Region">
                <input
                  ref={regionInputRef}
                  name="object_store_region"
                  value={objectStoreForm.region}
                  onChange={(event) =>
                    setObjectStoreForm((current) => ({
                      ...current,
                      region: event.target.value,
                    }))
                  }
                  className={`${INPUT_CLASS} font-mono`}
                  placeholder="us-east-1"
                  spellCheck={false}
                  autoCapitalize="off"
                />
              </Field>
              <CardPicker
                legend="Credentials"
                options={OBJECT_STORE_CREDENTIAL_MODE_OPTIONS}
                value={objectStoreForm.credentialMode}
                onChange={(credentialMode) => {
                  setObjectStoreForm((current) => ({ ...current, credentialMode }));
                  if (credentialMode === "access_key") {
                    focusInput(accessKeyIdInputRef);
                  }
                }}
              />
              {objectStoreForm.credentialMode === "access_key" ? (
                <div className="space-y-5">
                  <Field label="AWS access key ID">
                    <input
                      ref={accessKeyIdInputRef}
                      id="aws_access_key_id"
                      name="aws_access_key_id"
                      value={objectStoreForm.accessKeyId}
                      onChange={(event) =>
                        setObjectStoreForm((current) => ({
                          ...current,
                          accessKeyId: event.target.value,
                        }))
                      }
                      className={`${INPUT_CLASS} font-mono`}
                      placeholder="AKIA..."
                      spellCheck={false}
                      autoComplete="off"
                      autoCapitalize="off"
                    />
                  </Field>
                  <Field label="AWS secret access key">
                    <PasswordInput
                      inputRef={secretAccessKeyInputRef}
                      id="aws_secret_access_key"
                      name="aws_secret_access_key"
                      value={objectStoreForm.secretAccessKey}
                      onChange={(value) =>
                        setObjectStoreForm((current) => ({
                          ...current,
                          secretAccessKey: value,
                        }))
                      }
                      placeholder="Secret access key"
                    />
                  </Field>
                  {objectStoreForm.manualCredentialsSaved ? (
                    <p className="text-xs text-fg-muted">
                      Credentials saved. Leave both fields blank to keep them, or
                      enter both fields to replace them.
                    </p>
                  ) : null}
                </div>
              ) : (
                <p className="text-xs/5 text-fg-muted">
                  Fabro will use AWS credentials already provided by the runtime,
                  such as EC2, ECS, or IRSA-based auth.
                </p>
              )}
            </div>
          ) : (
            <div className="space-y-3">
              <Field
                label="Local directory"
                hint="Shared root for SlateDB and run artifacts."
              >
                <input
                  ref={localRootInputRef}
                  name="object_store_local_root"
                  value={objectStoreForm.localRoot}
                  onChange={(event) =>
                    setObjectStoreForm((current) => ({
                      ...current,
                      localRoot: event.target.value,
                    }))
                  }
                  className={`${INPUT_CLASS} font-mono`}
                  placeholder="Local object-store directory"
                  spellCheck={false}
                  autoCapitalize="off"
                />
              </Field>
              <p className="rounded-lg bg-overlay px-4 py-3 text-sm/6 text-fg-3 outline-1 -outline-offset-1 outline-white/10">
                Fabro will store SlateDB and run artifacts under this directory.
              </p>
            </div>
          )}
        </StepPanel>
      ) : location.pathname === "/install/sandbox" ? (
        <StepPanel
          title="Choose the sandbox runtime"
          description="Workflows run inside this sandbox. Docker uses the host daemon; Daytona runs each sandbox in its cloud."
          error={saveError}
          submitting={submitting}
          submittingLabel={
            sandboxForm.provider === "daytona" ? "Checking access..." : "Saving..."
          }
          backHref="/install/object-store"
          onSubmit={async () => {
            if (sandboxForm.provider === "daytona") {
              const apiKey = sandboxForm.apiKey.trim();
              const keepStoredKey = sandboxForm.apiKeySaved && !apiKey;
              if (!keepStoredKey && !apiKey) {
                setSaveError("Enter the Daytona API key before continuing.");
                focusInput(sandboxApiKeyInputRef);
                return;
              }
            }

            const payload = buildSandboxPayload(sandboxForm);
            await runStepSubmit({
              action: async () => {
                if (sandboxForm.provider === "daytona") {
                  await testInstallSandbox(installToken, payload);
                }
                await putInstallSandbox(installToken, payload);
              },
              fallback: "Failed to save sandbox settings.",
              next:     "/install/llm",
            });
          }}
        >
          <CardPicker
            legend="Sandbox runtime"
            options={SANDBOX_PROVIDER_OPTIONS}
            value={sandboxForm.provider}
            onChange={(provider) => {
              setSandboxForm((current) => ({ ...current, provider }));
              if (provider === "daytona") {
                focusInput(sandboxApiKeyInputRef);
              }
            }}
          />
          {sandboxForm.provider === "daytona" ? (
            <div className="space-y-5">
              <Field
                label="Daytona API key"
                hint={
                  sandboxForm.apiKeySaved
                    ? "A key is already saved. Leave blank to keep using it."
                    : "Stored in the vault and exported to workflows as DAYTONA_API_KEY."
                }
              >
                <input
                  ref={sandboxApiKeyInputRef}
                  type="password"
                  name="sandbox_api_key"
                  value={sandboxForm.apiKey}
                  onChange={(event) =>
                    setSandboxForm((current) => ({
                      ...current,
                      apiKey: event.target.value,
                    }))
                  }
                  className={`${INPUT_CLASS} font-mono`}
                  placeholder={
                    sandboxForm.apiKeySaved ? "•••• (saved)" : "dtn_..."
                  }
                  autoComplete="off"
                  spellCheck={false}
                />
              </Field>
            </div>
          ) : (
            <p className="rounded-lg bg-overlay px-4 py-3 text-sm/6 text-fg-3 outline-1 -outline-offset-1 outline-white/10">
              Fabro will use the host Docker daemon. Make sure the server has
              access to <code className="font-mono text-fg-2">/var/run/docker.sock</code>.
            </p>
          )}
        </StepPanel>
      ) : location.pathname === "/install/github/done" ? (
        <GithubAppDoneScreen github={session?.github} />
      ) : location.pathname === "/install/github" ? (
        <StepPanel
          title="Connect GitHub"
          description="Choose how Fabro should authenticate. Tokens are stored in the vault; apps hand off to GitHub and return here."
          error={saveError}
          submitting={submitting}
          submitLabel={githubStrategy === "app" ? "Continue on GitHub" : "Continue"}
          backHref="/install/llm"
          onSubmit={async () => {
            if (githubStrategy === "token") {
              const trimmedToken = tokenForm.token.trim();
              if (!trimmedToken) {
                setSaveError("Provide the GitHub token before continuing.");
                return;
              }
              await runStepSubmit({
                action: async () => {
                  const username = await testInstallGithubToken(installToken, trimmedToken);
                  setTokenForm({ token: trimmedToken, username });
                  await putInstallGithubToken(installToken, trimmedToken, username);
                },
                fallback: "Failed to start GitHub setup.",
                next:     "/install/review",
              });
              return;
            }

            const { owner, appName, allowedUsername } = appForm;
            if (owner.kind === "org" && !(owner.slug ?? "").trim()) {
              setSaveError("Enter the organization slug for the GitHub App.");
              return;
            }
            if (!appName.trim()) {
              setSaveError("Enter the GitHub App name before continuing.");
              return;
            }
            if (!allowedUsername.trim()) {
              setSaveError("Enter the GitHub username that should be allowed to log in.");
              return;
            }

            await runStepSubmit({
              action: async () => {
                const manifest = await createInstallGithubAppManifest(installToken, {
                  owner:
                    owner.kind === "org"
                      ? { kind: "org", slug: (owner.slug ?? "").trim() }
                      : { kind: "personal" },
                  app_name:         appName.trim(),
                  allowed_username: allowedUsername.trim(),
                });
                submitGithubManifest(
                  manifest.github_form_action,
                  manifest.manifest,
                  manifest.state,
                );
              },
              fallback: "Failed to start GitHub setup.",
            });
          }}
        >
          <CardPicker
            legend="Authentication"
            options={GITHUB_STRATEGY_OPTIONS}
            value={githubStrategy}
            onChange={(strategy) => setGithubStrategy(strategy)}
          />
          {githubStrategy === "token" ? (
            <div className="space-y-5">
              <div>
                <label
                  htmlFor="github_token"
                  className="text-sm font-medium text-fg"
                >
                  Personal access token
                </label>
                <div className="mt-2">
                  <PasswordInput
                    id="github_token"
                    name="github_token"
                    value={tokenForm.token}
                    onChange={(value) =>
                      setTokenForm((current) => ({ ...current, token: value }))
                    }
                    placeholder="ghp_..."
                  />
                </div>
                {tokenForm.username ? (
                  <p className="mt-2 inline-flex items-center gap-1.5 text-xs text-mint">
                    <CheckCircleIcon className="size-4 shrink-0" />
                    Previously validated as{" "}
                    <span className="font-medium">@{tokenForm.username}</span>
                  </p>
                ) : null}
                <HelpDisclosure summary="Where do I get this?">
                  <p>
                    Create a fine-grained or classic token with{" "}
                    <code className="font-mono text-fg-2">repo</code> scope.
                  </p>
                  <ExternalLink href="https://github.com/settings/tokens">
                    github.com/settings/tokens
                  </ExternalLink>
                </HelpDisclosure>
              </div>
            </div>
          ) : (
            <div className="space-y-5">
              <CardPicker
                legend="Owner"
                options={GITHUB_OWNER_OPTIONS}
                value={appForm.owner.kind}
                onChange={(kind) =>
                  setAppForm((current) => ({
                    ...current,
                    owner:
                      kind === "org"
                        ? { kind: "org", slug: current.owner.kind === "org" ? current.owner.slug ?? "" : "" }
                        : { kind: "personal" },
                  }))
                }
              />
              {appForm.owner.kind === "org" ? (
                <Field label="Organization slug">
                  <input
                    name="github_org_slug"
                    value={appForm.owner.slug ?? ""}
                    onChange={(event) =>
                      setAppForm((current) => ({
                        ...current,
                        owner: { kind: "org", slug: event.target.value },
                      }))
                    }
                    className={INPUT_CLASS}
                    placeholder="acme"
                    spellCheck={false}
                  />
                </Field>
              ) : null}
              <Field
                label="Allowed GitHub username"
                hint="Only this username can log in through GitHub after setup."
              >
                <input
                  name="github_allowed_username"
                  value={appForm.allowedUsername}
                  onChange={(event) =>
                    setAppForm((current) => ({
                      ...current,
                      allowedUsername: event.target.value,
                    }))
                  }
                  className={INPUT_CLASS}
                  placeholder="octocat"
                  spellCheck={false}
                />
              </Field>
              {session?.server?.canonical_url ? (
                <p className="text-xs text-fg-muted">
                  After creating the app, GitHub will redirect back to{" "}
                  <code className="font-mono text-fg-3">{session.server.canonical_url}</code>.
                </p>
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
    <main className="min-h-dvh bg-atmosphere px-4 py-16 text-fg-2 antialiased sm:py-20">
      <div className="relative mx-auto max-w-md">
        <div className="flex items-center gap-3">
          <img src="/logo.svg" alt="Fabro" className="size-8" draggable={false} />
          <span className="text-sm font-medium text-fg-3">Install</span>
        </div>
        <div className="mt-10">
          <h1 className="text-2xl font-semibold tracking-tight text-fg sm:text-[1.75rem]">
            Finish configuring this Fabro server
          </h1>
          <p className="mt-3 max-w-[56ch] text-sm/6 text-fg-3 text-pretty">
            Find the one-time install token in your terminal, Docker logs, or
            platform log viewer, then paste it here to continue.
          </p>
        </div>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            onSubmit();
          }}
          className="mt-8 space-y-5"
        >
          <div>
            <label htmlFor="install-token" className="sr-only">
              Install token
            </label>
            <input
              id="install-token"
              type="password"
              name="install_token"
              value={manualToken}
              onChange={(event) => setManualToken(event.target.value)}
              className={`${INPUT_CLASS} font-mono`}
              placeholder="Paste install token"
              spellCheck={false}
              autoComplete="off"
              autoCapitalize="off"
              autoFocus
            />
          </div>
          {sessionError ? <ErrorMessage message={sessionError} /> : null}
          <button type="submit" className={PRIMARY_BUTTON_CLASS}>
            Continue
            <ArrowRightIcon className="size-4 shrink-0" />
          </button>
        </form>
        <section className="mt-10 border-t border-line pt-6">
          <h2 className="text-xs font-semibold tracking-wide text-fg uppercase">
            Where to find it
          </h2>
          <dl className="mt-4 space-y-2 text-sm/6 text-fg-3">
            <div className="flex gap-3">
              <dt className="w-24 shrink-0 text-fg-2">Local</dt>
              <dd>
                Output of{" "}
                <code className="font-mono text-fg-2">fabro server start</code>
              </dd>
            </div>
            <div className="flex gap-3">
              <dt className="w-24 shrink-0 text-fg-2">Docker</dt>
              <dd>
                <code className="font-mono text-fg-2">docker logs &lt;container&gt;</code>
              </dd>
            </div>
            <div className="flex gap-3">
              <dt className="w-24 shrink-0 text-fg-2">Hosted</dt>
              <dd>Your platform's log viewer or <code className="font-mono text-fg-2">journalctl</code></dd>
            </div>
          </dl>
        </section>
        <p className="mt-10 text-xs text-fg-muted">
          Install mode is temporary and only available until setup completes.
        </p>
      </div>
    </main>
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
  const showStepper = currentStep !== "welcome";
  return (
    <main className="min-h-dvh bg-atmosphere px-4 py-12 text-fg-2 antialiased sm:py-16">
      <div className="relative mx-auto max-w-xl">
        <div className="flex items-center gap-3">
          <img src="/logo.svg" alt="Fabro" className="size-8" draggable={false} />
          <span className="text-sm font-medium text-fg-3">Install</span>
        </div>
        {showStepper ? (
          <div className="mt-8">
            <Stepper currentStep={currentStep} completedSteps={completedSteps} />
          </div>
        ) : null}
        <div className="mt-10 sm:mt-12">{children}</div>
      </div>
    </main>
  );
}

function Stepper({
  currentStep,
  completedSteps,
}: {
  currentStep: StepId;
  completedSteps: Set<string>;
}) {
  const activeIndex = STEPPER_STEPS.findIndex((step) => step.id === currentStep);
  const safeIndex = activeIndex === -1 ? 0 : activeIndex;
  const activeStep = STEPPER_STEPS[safeIndex];
  const progress = ((safeIndex + 1) / STEPPER_STEPS.length) * 100;

  return (
    <nav aria-label="Install progress">
      <div className="sm:hidden">
        <p className="text-xs font-medium text-fg-3 tabular-nums">
          Step {safeIndex + 1} of {STEPPER_STEPS.length}
          <span className="text-fg"> · {activeStep.label}</span>
        </p>
        <div className="mt-2 h-1 overflow-hidden rounded-full bg-overlay">
          <div
            className="h-full rounded-full bg-teal-500 transition-[width]"
            style={{ width: `${progress}%` }}
          />
        </div>
      </div>
      <ol role="list" className="hidden items-center sm:flex">
        {STEPPER_STEPS.map((step, index) => {
          const isComplete = completedSteps.has(step.id);
          const isCurrent = step.id === currentStep;
          const isLast = index === STEPPER_STEPS.length - 1;
          const isLinkable = isComplete || isCurrent;
          const circleClass = isComplete
            ? "bg-mint text-on-primary"
            : isCurrent
              ? "bg-teal-500 text-on-primary"
              : "bg-overlay text-fg-muted outline-1 -outline-offset-1 outline-white/10";
          const labelClass = isCurrent
            ? "text-fg"
            : isComplete
              ? "text-fg-2"
              : "text-fg-muted";
          const connectorClass = isComplete ? "bg-mint/40" : "bg-line-strong";
          const inner = (
            <>
              <span
                className={`flex size-6 items-center justify-center rounded-full text-xs font-semibold tabular-nums ${circleClass}`}
                aria-hidden="true"
              >
                {isComplete ? <CheckIcon className="size-3.5" /> : index + 1}
              </span>
              <span className={`text-xs font-medium ${labelClass}`}>
                {step.label}
              </span>
            </>
          );
          return (
            <li
              key={step.id}
              className={`flex items-center ${isLast ? "" : "flex-1"}`}
            >
              {isLinkable ? (
                <Link
                  to={step.href}
                  aria-current={isCurrent ? "step" : undefined}
                  className="flex items-center gap-2 rounded-md outline-teal-500 focus-visible:outline-2 focus-visible:outline-offset-4"
                >
                  {inner}
                </Link>
              ) : (
                <span className="flex items-center gap-2" aria-disabled="true">
                  {inner}
                </span>
              )}
              {isLast ? null : (
                <span
                  aria-hidden="true"
                  className={`mx-3 h-px flex-1 ${connectorClass}`}
                />
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

function WelcomeScreen() {
  return (
    <div>
      <h1 className="text-3xl font-semibold tracking-tight text-fg text-balance sm:text-4xl">
        Set up your Fabro server
      </h1>
      <p className="mt-4 max-w-[56ch] text-base/7 text-fg-3 text-pretty sm:text-[0.9375rem]/7">
        A short walkthrough to confirm the public server URL, choose the shared
        object store and sandbox runtime, validate your LLM credentials, and
        connect GitHub. When you finish, Fabro restarts into normal mode.
      </p>
      <ol role="list" className="mt-10 divide-y divide-line border-y border-line">
        {[
          ["Server URL", "Confirm where operators will reach Fabro."],
          [
            "Object store",
            "Choose local disk or AWS S3 for SlateDB and artifacts.",
          ],
          ["Sandbox", "Choose Docker or Daytona for workflow execution."],
          ["LLMs", "Validate API keys for Anthropic, OpenAI, or Gemini."],
          ["GitHub", "Choose a personal access token or a GitHub App."],
          ["Review", "Double-check the plan, then write the files."],
        ].map(([title, body], index) => (
          <li key={title} className="flex items-start gap-4 py-4">
            <span
              className="mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full bg-overlay text-xs font-semibold tabular-nums text-fg-2 outline-1 -outline-offset-1 outline-white/10"
              aria-hidden="true"
            >
              {index + 1}
            </span>
            <div>
              <p className="text-sm font-medium text-fg">{title}</p>
              <p className="mt-1 text-sm/6 text-fg-3">{body}</p>
            </div>
          </li>
        ))}
      </ol>
      <div className="mt-10 flex justify-end">
        <Link to="/install/server" className={PRIMARY_BUTTON_CLASS}>
          Start setup
          <ArrowRightIcon className="size-4 shrink-0" />
        </Link>
      </div>
    </div>
  );
}

function StepPanel({
  title,
  description,
  children,
  error,
  submitting,
  submitLabel = "Continue",
  submittingLabel = "Saving...",
  backHref,
  onSubmit,
}: {
  title: string;
  description: string;
  children: ReactNode;
  error: string | null;
  submitting: boolean;
  submitLabel?: string;
  submittingLabel?: string;
  backHref?: string;
  onSubmit: () => Promise<void>;
}) {
  return (
    <form
      onSubmit={(event: FormEvent<HTMLFormElement>) => {
        event.preventDefault();
        if (submitting) return;
        void onSubmit();
      }}
      className="space-y-8"
    >
      <header>
        <h1 className="text-2xl font-semibold tracking-tight text-fg text-balance sm:text-[1.75rem]">
          {title}
        </h1>
        <p className="mt-3 max-w-[56ch] text-sm/6 text-fg-3 text-pretty">
          {description}
        </p>
      </header>
      <div className="space-y-5">{children}</div>
      {error ? <ErrorMessage message={error} /> : null}
      <div className="flex items-center justify-between gap-3 pt-2">
        {backHref ? (
          <Link to={backHref} className={SECONDARY_BUTTON_CLASS}>
            <ArrowLeftIcon className="size-4 shrink-0" />
            Back
          </Link>
        ) : (
          <span />
        )}
        <button type="submit" disabled={submitting} className={PRIMARY_BUTTON_CLASS}>
          {submitting ? (
            <>
              <Spinner />
              {submittingLabel}
            </>
          ) : (
            <>
              {submitLabel}
              <ArrowRightIcon className="size-4 shrink-0" />
            </>
          )}
        </button>
      </div>
    </form>
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
  const providers = (session?.llm?.providers ?? [])
    .map((provider) => describeProvider(provider.provider))
    .join(", ");
  const serverUrl =
    session?.server?.canonical_url || session?.prefill.canonical_url || "Unknown";
  return (
    <form
      onSubmit={(event: FormEvent<HTMLFormElement>) => {
        event.preventDefault();
        if (submitting) return;
        void onInstall();
      }}
      className="space-y-8"
    >
      <header>
        <h1 className="text-2xl font-semibold tracking-tight text-fg text-balance sm:text-[1.75rem]">
          Review and install
        </h1>
        <p className="mt-3 max-w-[56ch] text-sm/6 text-fg-3 text-pretty">
          Confirm the plan below. Fabro writes the configuration to disk, then
          restarts into normal mode.
        </p>
      </header>
      <dl className="divide-y divide-line border-y border-line">
        <SummaryRow
          label="Server URL"
          value={serverUrl}
          mono
          action={<CopyButton value={serverUrl} label="Copy server URL" />}
        />
        {renderObjectStoreSummaryRows(session?.object_store)}
        {renderSandboxSummaryRows(session?.sandbox)}
        <SummaryRow label="LLM providers" value={providers || "Not configured"} />
        {renderGithubSummaryRows(session?.github, serverUrl)}
      </dl>
      {error ? <ErrorMessage message={error} /> : null}
      <div className="flex items-center justify-between gap-3 pt-2">
        <Link to="/install/github" className={SECONDARY_BUTTON_CLASS}>
          <ArrowLeftIcon className="size-4 shrink-0" />
          Back
        </Link>
        <button type="submit" disabled={submitting} className={PRIMARY_BUTTON_CLASS}>
          {submitting ? (
            <>
              <Spinner />
              Installing
            </>
          ) : (
            "Install"
          )}
        </button>
      </div>
    </form>
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
    <div className="space-y-8">
      <header>
        <h1 className="text-2xl font-semibold tracking-tight text-fg text-balance sm:text-[1.75rem]">
          {timedOut ? "Install complete" : "Finishing up"}
        </h1>
        <p className="mt-3 max-w-[56ch] text-sm/6 text-fg-3 text-pretty">
          {timedOut
            ? "The server didn't come back automatically. Start it manually and return to the URL below."
            : "Configuration written. Waiting for the server to restart into normal mode."}
        </p>
      </header>
      {timedOut ? (
        <div className="rounded-lg bg-overlay px-4 py-3 text-sm/6 text-fg-2 outline-1 -outline-offset-1 outline-amber/30">
          Run <code className="font-mono text-fg">fabro server start</code>, then
          visit{" "}
          <code className="font-mono text-fg">{finishState.restart_url}</code>.
        </div>
      ) : (
        <div className="flex items-center gap-3 rounded-lg bg-overlay px-4 py-3 outline-1 -outline-offset-1 outline-white/10">
          <Spinner className="text-teal-300" />
          <p className="text-sm/6 text-fg-3">
            Polling <code className="font-mono text-fg-2">/health</code>…
          </p>
        </div>
      )}
      {finishState.dev_token ? (
        <div className="rounded-lg bg-overlay p-4 outline-1 -outline-offset-1 outline-white/10">
          <p className="text-xs font-semibold tracking-wide text-fg uppercase">
            Development token
          </p>
          <p className="mt-1 text-sm/6 text-fg-3">
            Use this to sign in after the server restarts.
          </p>
          <div className="mt-3">
            <CopyableToken token={finishState.dev_token} />
          </div>
        </div>
      ) : null}
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
    <div className="space-y-6">
      {INSTALL_PROVIDERS.map((provider) => {
        const current = value[provider.id] ?? { apiKey: "" };
        return (
          <div key={provider.id}>
            <label
              htmlFor={`${provider.id}_api_key`}
              className="text-sm font-medium text-fg"
            >
              {provider.label}
            </label>
            <div className="mt-2">
              <PasswordInput
                id={`${provider.id}_api_key`}
                name={`${provider.id}_api_key`}
                value={current.apiKey}
                onChange={(next) =>
                  onChange({
                    ...value,
                    [provider.id]: { ...current, apiKey: next },
                  })
                }
                placeholder={provider.envVar}
              />
            </div>
            <HelpDisclosure summary="Where do I get this?">
              <p>{provider.keyHelp.text}</p>
              <ExternalLink href={provider.keyHelp.url}>
                {provider.keyHelp.url.replace(/^https?:\/\//, "")}
              </ExternalLink>
            </HelpDisclosure>
          </div>
        );
      })}
    </div>
  );
}

type CardOption<T extends string> = { id: T; title: string; body: string };

function CardPicker<T extends string>({
  legend,
  options,
  value,
  onChange,
}: {
  legend:   string;
  options:  ReadonlyArray<CardOption<T>>;
  value:    T;
  onChange: (value: T) => void;
}) {
  return (
    <fieldset>
      <legend className="text-sm font-medium text-fg">{legend}</legend>
      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        {options.map((option) => (
          <OptionCard
            key={option.id}
            selected={value === option.id}
            onSelect={() => onChange(option.id)}
            title={option.title}
            body={option.body}
          />
        ))}
      </div>
    </fieldset>
  );
}

const GITHUB_STRATEGY_OPTIONS: ReadonlyArray<CardOption<GithubStrategy>> = [
  {
    id:    "token",
    title: "Personal access token",
    body:  "Quickest path. Validates a PAT and stores it in the vault.",
  },
  {
    id:    "app",
    title: "GitHub App",
    body:  "Recommended for teams. Enables OAuth.",
  },
];

const OBJECT_STORE_PROVIDER_OPTIONS: ReadonlyArray<CardOption<ObjectStoreProvider>> = [
  {
    id:    "local",
    title: "Local disk",
    body:  "Uses the host filesystem for SlateDB and run artifacts.",
  },
  {
    id:    "s3",
    title: "AWS S3",
    body:  "Uses one S3 bucket with fixed slatedb/ and artifacts/ prefixes.",
  },
];

const OBJECT_STORE_CREDENTIAL_MODE_OPTIONS: ReadonlyArray<
  CardOption<ObjectStoreCredentialMode>
> = [
  {
    id:    "runtime",
    title: "Use AWS runtime credentials",
    body:  "Use credentials already supplied by the deployment environment.",
  },
  {
    id:    "access_key",
    title: "Enter AWS access key credentials",
    body:  "Store an access key pair in server.env for startup and validation.",
  },
];

const SANDBOX_PROVIDER_OPTIONS: ReadonlyArray<CardOption<SandboxProvider>> = [
  {
    id:    "docker",
    title: "Docker",
    body:  "Default. Uses the host Docker daemon to run sandbox containers.",
  },
  {
    id:    "daytona",
    title: "Daytona",
    body:  "Each run gets a managed Daytona cloud sandbox. Requires an API key.",
  },
];

const GITHUB_OWNER_OPTIONS: ReadonlyArray<CardOption<GithubOwnerKind>> = [
  {
    id:    "personal",
    title: "Personal account",
    body:  "GitHub's personal app creation flow.",
  },
  {
    id:    "org",
    title: "Organization",
    body:  "GitHub's org flow — requires the org slug.",
  },
];

function OptionCard({
  selected,
  onSelect,
  title,
  body,
}: {
  selected: boolean;
  onSelect: () => void;
  title: string;
  body: string;
}) {
  const base =
    "group relative flex items-start gap-3 rounded-lg px-4 py-3.5 text-left outline-1 -outline-offset-1 transition-colors";
  const state = selected
    ? "bg-teal-500/10 outline-teal-500/60"
    : "bg-overlay outline-white/10 hover:bg-overlay-strong hover:outline-white/15";
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`${base} ${state}`}
    >
      <span
        aria-hidden="true"
        className={`mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full outline-1 -outline-offset-1 ${
          selected
            ? "bg-teal-500 outline-teal-500"
            : "bg-transparent outline-white/20"
        }`}
      >
        {selected ? (
          <span className="size-1.5 rounded-full bg-navy-950" />
        ) : null}
      </span>
      <span className="min-w-0">
        <span className="block text-sm font-medium text-fg">{title}</span>
        <span className="mt-1 block text-xs/5 text-fg-3">{body}</span>
      </span>
    </button>
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
    <div className="space-y-8">
      <header>
        <h1 className="text-2xl font-semibold tracking-tight text-fg text-balance sm:text-[1.75rem]">
          GitHub App connected
        </h1>
        <p className="mt-3 max-w-[56ch] text-sm/6 text-fg-3 text-pretty">
          The app credentials are staged. They'll be written into the runtime
          env file when the install finishes.
        </p>
      </header>
      <dl className="divide-y divide-line border-y border-line">
        <SummaryRow label="Owner" value={describeGithubAppOwner(github.owner)} />
        <SummaryRow
          label="App"
          value={github.slug || github.app_name || "GitHub App"}
          mono
        />
        <SummaryRow
          label="Allowed user"
          value={github.allowed_username || "Unknown"}
          mono
        />
      </dl>
      <div className="flex justify-end">
        <Link to="/install/review" className={PRIMARY_BUTTON_CLASS}>
          Continue to review
          <ArrowRightIcon className="size-4 shrink-0" />
        </Link>
      </div>
    </div>
  );
}

function SummaryRow({
  label,
  value,
  mono,
  action,
}: {
  label: string;
  value: string;
  mono?: boolean;
  action?: ReactNode;
}) {
  return (
    <div className="grid grid-cols-3 gap-4 py-4">
      <dt className="text-sm text-fg-3">{label}</dt>
      <dd className="col-span-2 flex items-start gap-2">
        <span className={`min-w-0 flex-1 text-sm text-fg break-words ${mono ? "font-mono" : ""}`}>
          {value}
        </span>
        {action}
      </dd>
    </div>
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
    <label className="block">
      <div className="flex flex-col gap-1 sm:flex-row sm:items-baseline sm:justify-between sm:gap-4">
        <span className="text-sm font-medium text-fg">{label}</span>
        {hint ? <span className="text-xs text-fg-muted">{hint}</span> : null}
      </div>
      <div className="mt-2">{children}</div>
    </label>
  );
}

function PasswordInput({
  id,
  name,
  value,
  onChange,
  placeholder,
  inputRef,
}: {
  id?: string;
  name: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  inputRef?: Ref<HTMLInputElement>;
}) {
  const [visible, setVisible] = useState(false);
  return (
    <div className="relative">
      <input
        ref={inputRef}
        type={visible ? "text" : "password"}
        id={id}
        name={name}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className={`${INPUT_CLASS} pr-11 font-mono`}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        autoCapitalize="off"
      />
      <button
        type="button"
        onClick={() => setVisible((current) => !current)}
        className="absolute inset-y-0 right-0 flex items-center rounded-r-lg px-3 text-fg-muted outline-teal-500 hover:text-fg-2 focus-visible:outline-2 focus-visible:-outline-offset-2"
        aria-label={visible ? "Hide value" : "Show value"}
      >
        {visible ? (
          <EyeSlashIcon className="size-4" />
        ) : (
          <EyeIcon className="size-4" />
        )}
      </button>
    </div>
  );
}

function CopyableToken({ token }: { token: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="flex items-stretch gap-2">
      <pre className="flex-1 overflow-x-auto rounded-md bg-panel-alt px-3 py-2 font-mono text-sm text-fg-2 outline-1 -outline-offset-1 outline-white/10">
        <code>{token}</code>
      </pre>
      <button
        type="button"
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(token);
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1500);
          } catch {
            // Clipboard may be blocked; leave state unchanged.
          }
        }}
        className="inline-flex items-center gap-1.5 rounded-md bg-overlay px-3 text-xs font-medium text-fg-2 outline-1 -outline-offset-1 outline-white/10 hover:bg-overlay-strong focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500"
        aria-label={copied ? "Copied" : "Copy token"}
      >
        {copied ? (
          <ClipboardDocumentCheckIcon className="size-4 text-mint" />
        ) : (
          <ClipboardIcon className="size-4" />
        )}
        <span>{copied ? "Copied" : "Copy"}</span>
      </button>
    </div>
  );
}

function HelpDisclosure({
  summary,
  children,
}: {
  summary: string;
  children: ReactNode;
}) {
  return (
    <details className="group mt-2">
      <summary className="inline-flex list-none items-center gap-1 rounded text-xs text-fg-3 outline-teal-500 select-none hover:text-fg-2 focus-visible:outline-2 focus-visible:outline-offset-2 [&::-webkit-details-marker]:hidden">
        <ChevronDownIcon className="size-3.5 shrink-0 transition-transform group-open:-rotate-180" />
        <span>{summary}</span>
      </summary>
      <div className="mt-2 space-y-1.5 text-xs/5 text-fg-3">{children}</div>
    </details>
  );
}

function ExternalLink({
  href,
  children,
}: {
  href: string;
  children: ReactNode;
}) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className="inline-flex items-center gap-1 font-mono text-teal-300 hover:text-teal-500 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 rounded"
    >
      {children}
      <ArrowTopRightOnSquareIcon className="size-3 shrink-0" />
    </a>
  );
}

function Spinner({ className = "" }: { className?: string }) {
  return (
    <svg
      className={`size-4 shrink-0 animate-spin ${className}`}
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
    >
      <circle cx="8" cy="8" r="6" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2" />
      <path
        d="M14 8a6 6 0 0 0-6-6"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
      />
    </svg>
  );
}

function defaultProviderSelection(): ProviderSelection {
  return Object.fromEntries(
    INSTALL_PROVIDERS.map((provider) => [provider.id, { apiKey: "" }]),
  );
}

function defaultObjectStoreForm(localRoot = ""): ObjectStoreForm {
  return {
    provider: "local",
    localRoot,
    bucket: "",
    region: "",
    credentialMode: "runtime",
    accessKeyId: "",
    secretAccessKey: "",
    manualCredentialsSaved: false,
  };
}

function hydrateProviderSelection(
  current: ProviderSelection,
  session: InstallSessionResponse,
): ProviderSelection {
  const hasUserInput = Object.values(current).some((provider) => provider.apiKey);
  if (hasUserInput) return current;

  const next = defaultProviderSelection();
  for (const provider of session.llm?.providers ?? []) {
    next[provider.provider] = { apiKey: "" };
  }
  return next;
}

function hydrateObjectStoreForm(session: InstallSessionResponse): ObjectStoreForm {
  const summary = session.object_store;
  if (!summary || summary.provider === "local") {
    return defaultObjectStoreForm(
      summary?.root ?? session.prefill.object_store_local_root,
    );
  }
  return {
    provider: "s3",
    localRoot: session.prefill.object_store_local_root,
    bucket: summary.bucket ?? "",
    region: summary.region ?? "",
    credentialMode: summary.credential_mode === "access_key" ? "access_key" : "runtime",
    accessKeyId: "",
    secretAccessKey: "",
    manualCredentialsSaved: Boolean(summary.manual_credentials_saved),
  };
}

function buildObjectStorePayload(form: ObjectStoreForm): InstallObjectStoreInput {
  if (form.provider === "local") {
    return { provider: "local", root: form.localRoot.trim() };
  }

  const payload: InstallObjectStoreInput = {
    provider: "s3",
    bucket: form.bucket.trim(),
    region: form.region.trim(),
    credential_mode: form.credentialMode,
  };
  const accessKeyId = form.accessKeyId.trim();
  const secretAccessKey = form.secretAccessKey.trim();
  if (form.credentialMode === "access_key") {
    if (accessKeyId) {
      payload.access_key_id = accessKeyId;
    }
    if (secretAccessKey) {
      payload.secret_access_key = secretAccessKey;
    }
  }
  return payload;
}

function defaultSandboxForm(): SandboxForm {
  return { provider: "docker", apiKey: "", apiKeySaved: false };
}

function hydrateSandboxForm(
  current: SandboxForm,
  session: InstallSessionResponse,
): SandboxForm {
  const summary = session.sandbox;
  if (!summary) {
    return current.apiKey ? { ...current, apiKeySaved: false } : defaultSandboxForm();
  }
  if (current.apiKey) {
    return { ...current, apiKeySaved: Boolean(summary.api_key_saved) };
  }
  return {
    provider:    summary.provider === "daytona" ? "daytona" : "docker",
    apiKey:      "",
    apiKeySaved: Boolean(summary.api_key_saved),
  };
}

function buildSandboxPayload(form: SandboxForm): InstallSandboxInput {
  if (form.provider === "docker") {
    return { provider: "docker" };
  }
  const apiKey = form.apiKey.trim();
  const payload: InstallSandboxInput = { provider: "daytona" };
  if (apiKey) {
    payload.api_key = apiKey;
  }
  return payload;
}

function focusInput(ref: { current: HTMLInputElement | null }): void {
  window.setTimeout(() => ref.current?.focus(), 0);
}

function describeProvider(id: string): string {
  const match = INSTALL_PROVIDERS.find((provider) => provider.id === id);
  return match?.label ?? id;
}

function renderGithubSummaryRows(
  github: InstallSessionResponse["github"],
  serverUrl: string,
): ReactNode {
  if (!github) {
    return <SummaryRow label="GitHub" value="Not configured" />;
  }
  if (github.strategy === "app") {
    return (
      <>
        <SummaryRow label="GitHub connection" value="GitHub App" />
        <SummaryRow label="App owner" value={describeGithubAppOwner(github.owner)} />
        <SummaryRow
          label="Allowed user"
          value={github.allowed_username ? `@${github.allowed_username}` : "Not set"}
          mono={Boolean(github.allowed_username)}
        />
        <SummaryRow
          label="GitHub callback URL"
          value={githubCallbackUrl(serverUrl)}
          mono
        />
      </>
    );
  }
  return (
    <>
      <SummaryRow label="GitHub connection" value="Personal access token" />
      <SummaryRow
        label="User"
        value={github.username ? `@${github.username}` : "Not set"}
        mono={Boolean(github.username)}
      />
    </>
  );
}

function githubCallbackUrl(serverUrl: string): string {
  return `${serverUrl.replace(/\/+$/, "")}/auth/callback/github`;
}

function renderObjectStoreSummaryRows(
  objectStore: InstallSessionResponse["object_store"],
): ReactNode {
  if (!objectStore) {
    return <SummaryRow label="Object store" value="Not configured" />;
  }
  if (objectStore.provider === "local") {
    return (
      <>
        <SummaryRow label="Object store" value="Local disk" />
        <SummaryRow label="Directory" value={objectStore.root ?? "Not set"} mono />
      </>
    );
  }
  return (
    <>
      <SummaryRow label="Object store" value="AWS S3" />
      <SummaryRow label="Bucket" value={objectStore.bucket ?? "Not set"} mono />
      <SummaryRow label="Region" value={objectStore.region ?? "Not set"} mono />
      <SummaryRow
        label="Credentials"
        value={
          objectStore.credential_mode === "access_key"
            ? "Access key"
            : "Runtime credentials"
        }
      />
      <SummaryRow label="Prefixes" value="slatedb/, artifacts/" mono />
    </>
  );
}

function renderSandboxSummaryRows(
  sandbox: InstallSessionResponse["sandbox"],
): ReactNode {
  if (!sandbox) {
    return <SummaryRow label="Sandbox" value="Not configured" />;
  }
  if (sandbox.provider === "daytona") {
    return (
      <>
        <SummaryRow label="Sandbox" value="Daytona" />
        <SummaryRow
          label="Daytona API key"
          value={sandbox.api_key_saved ? "Saved" : "Not set"}
        />
      </>
    );
  }
  return <SummaryRow label="Sandbox" value="Docker" />;
}

function describeGithubAppOwner(
  owner: InstallGithubAppOwner | undefined,
): string {
  if (!owner || owner.kind === "personal") return "Personal account";
  return owner.slug ? `@${owner.slug} (organization)` : "Organization";
}

function submitGithubManifest(
  formAction: string,
  manifest: Record<string, unknown>,
  state: string,
): void {
  const form = document.createElement("form");
  form.method = "post";
  form.action = formAction;
  form.style.display = "none";

  const manifestInput = document.createElement("input");
  manifestInput.type = "hidden";
  manifestInput.name = "manifest";
  manifestInput.value = JSON.stringify(manifest);
  form.appendChild(manifestInput);

  const stateInput = document.createElement("input");
  stateInput.type = "hidden";
  stateInput.name = "state";
  stateInput.value = state;
  form.appendChild(stateInput);

  document.body.appendChild(form);
  form.submit();
}
