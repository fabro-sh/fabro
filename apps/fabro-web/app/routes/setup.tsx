import { AuthLayout } from "../components/auth-layout";

export default function Setup() {
  return (
    <AuthLayout footer="GitHub App setup is managed from the terminal, not the browser.">
      <h1 className="text-center text-lg font-semibold text-fg">
        Set up Fabro
      </h1>
      <p className="mt-2 text-center text-sm text-fg-3">
        Run the installer on the same host that runs the Fabro server to
        register a GitHub App and write local configuration.
      </p>
      <div className="mt-6 space-y-4">
        <div className="rounded-lg border border-line-strong bg-overlay px-4 py-3">
          <p className="text-xs font-medium uppercase tracking-wide text-fg-muted">
            1. Open a terminal on the server host
          </p>
          <pre className="mt-2 overflow-x-auto text-sm text-fg-2">
            <code>fabro install</code>
          </pre>
        </div>
        <div className="rounded-lg border border-line-strong bg-overlay px-4 py-3">
          <p className="text-xs font-medium uppercase tracking-wide text-fg-muted">
            2. Choose GitHub App setup
          </p>
          <p className="mt-2 text-sm text-fg-3">
            The CLI opens GitHub, exchanges the manifest code, and writes the
            required settings and secrets locally.
          </p>
        </div>
        <div className="rounded-lg border border-line-strong bg-overlay px-4 py-3">
          <p className="text-xs font-medium uppercase tracking-wide text-fg-muted">
            3. Restart the server, then return to sign in
          </p>
          <a
            href="/login"
            className="mt-3 flex w-full items-center justify-center rounded-lg bg-teal-500 px-4 py-2.5 text-sm font-medium text-white transition-colors hover:bg-teal-300"
          >
            Continue to sign in
          </a>
        </div>
      </div>
    </AuthLayout>
  );
}
