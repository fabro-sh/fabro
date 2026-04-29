import { useLocation } from "react-router";

import { AuthLayout } from "../components/auth-layout";
import { PRIMARY_BUTTON_CLASS } from "../components/ui";

const firstTimeSteps = [
  {
    title: "Open a terminal on the server host",
    body: (
      <p className="text-sm/6 text-fg-3">
        Run{" "}
        <code className="font-mono text-fg-2">fabro install</code> on the same
        host that runs the Fabro server.
      </p>
    ),
  },
  {
    title: "Choose GitHub App setup",
    body: (
      <p className="text-sm/6 text-fg-3">
        The CLI opens GitHub, exchanges the manifest code, and writes the
        required settings and secrets locally.
      </p>
    ),
  },
  {
    title: "Restart the server, then return to sign in",
    body: (
      <p className="text-sm/6 text-fg-3">
        Once the server comes back up, you can authenticate from the browser.
      </p>
    ),
  },
];

const githubInstallReturnSteps = [
  {
    title: "Return to Fabro",
    body: (
      <p className="text-sm/6 text-fg-3">
        The GitHub App is installed for the selected account or repositories.
        No local reinstall is needed.
      </p>
    ),
  },
  {
    title: "Retry the run",
    body: (
      <p className="text-sm/6 text-fg-3">
        Start the run or preflight again so Fabro can clone the repository and
        push checkpoint branches with the new installation.
      </p>
    ),
  },
];

export function setupContentForSearch(search: string) {
  const params = new URLSearchParams(search);
  if (params.has("installation_id") || params.get("setup_action") === "install") {
    return {
      footer:
        "GitHub redirected here after installing the app. Fabro is already configured locally.",
      title: "GitHub App installed",
      description:
        "GitHub finished installing the app. Fabro can now request repository-scoped tokens for runs that use that installation.",
      steps: githubInstallReturnSteps,
      cta: "Continue to sign in",
    };
  }

  return {
    footer: "GitHub App setup is managed from the terminal, not the browser.",
    title: "Set up Fabro",
    description:
      "Run the installer on the server host to register a GitHub App and write local configuration.",
    steps: firstTimeSteps,
    cta: "Continue to sign in",
  };
}

export default function Setup() {
  const { search } = useLocation();
  const content = setupContentForSearch(search);

  return (
    <AuthLayout footer={content.footer}>
      <h1 className="text-center text-2xl font-semibold tracking-tight text-fg text-balance sm:text-[1.75rem]">
        {content.title}
      </h1>
      <p className="mt-3 text-center text-sm/6 text-fg-3 text-pretty">
        {content.description}
      </p>
      <ol
        role="list"
        className="mt-8 divide-y divide-line border-y border-line"
      >
        {content.steps.map((step, index) => (
          <li key={step.title} className="flex items-start gap-4 py-4">
            <span
              className="mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full bg-overlay text-xs font-semibold tabular-nums text-fg-2 outline-1 -outline-offset-1 outline-white/10"
              aria-hidden="true"
            >
              {index + 1}
            </span>
            <div className="min-w-0">
              <p className="text-sm font-medium text-fg">{step.title}</p>
              <div className="mt-1">{step.body}</div>
            </div>
          </li>
        ))}
      </ol>
      <a href="/login" className={`${PRIMARY_BUTTON_CLASS} mt-8 w-full`}>
        {content.cta}
      </a>
    </AuthLayout>
  );
}
