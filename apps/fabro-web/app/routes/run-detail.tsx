import { useEffect } from "react";
import { ChevronDownIcon, ChevronRightIcon } from "@heroicons/react/20/solid";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import { Link, Outlet, useFetcher, useLocation } from "react-router";
import { mapRunSummaryToRunItem, runStatusDisplay, isRunStatus } from "../data/runs";
import type { RunSummaryResponse } from "../data/runs";
import { apiJson } from "../api";
import { useDemoMode } from "../lib/demo-mode";
import type { PreviewUrlResponse } from "@qltysh/fabro-api-client";

const allTabs = [
  { name: "Overview", path: "", count: null, demoOnly: false, broken: false },
  { name: "Stages", path: "/stages", count: null, demoOnly: false, broken: false },
  { name: "Files Changed", path: "/files", count: null, demoOnly: false, broken: false },
  { name: "Graph", path: "/graph", count: null, demoOnly: false, broken: false },
  { name: "Billing", path: "/billing", count: null, demoOnly: false, broken: false },
];

export const handle = { hideHeader: true };

export async function loader({ request, params }: any) {
  const response = await fetch(`/api/v1/runs/${params.id}`, {
    credentials: "include",
  });
  if (!response.ok) return { run: null };
  const summary: RunSummaryResponse = await response.json();
  const item = mapRunSummaryToRunItem(summary);
  const rawStatus = summary.status;
  const display = isRunStatus(rawStatus)
    ? runStatusDisplay[rawStatus]
    : { label: rawStatus, dot: "bg-fg-muted", text: "text-fg-muted" };
  return {
    run: {
      ...item,
      statusLabel: display.label,
      statusDot: display.dot,
      statusText: display.text,
    },
  };
}

export async function action({ params, request }: any) {
  const formData = await request.formData();
  const port = formData.get("port");
  const expiresInSecs = formData.get("expires_in_secs");
  const result = await apiJson<PreviewUrlResponse>(`/runs/${params.id}/preview`, {
    request,
    init: {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ port: Number(port), expires_in_secs: Number(expiresInSecs) }),
    },
  });
  return result;
}

export function meta({ data }: any) {
  const run = data?.run;
  return [{ title: run ? `${run.title} — Fabro` : "Run — Fabro" }];
}

export default function RunDetail({ loaderData, params }: any) {
  const { run } = loaderData;
  const { pathname } = useLocation();
  const basePath = `/runs/${params.id}`;
  const previewFetcher = useFetcher<PreviewUrlResponse>();
  const demoMode = useDemoMode();
  const tabs = allTabs.filter((t) => !t.broken && (!t.demoOnly || demoMode));

  useEffect(() => {
    if (previewFetcher.data?.url) {
      window.open(previewFetcher.data.url, "_blank");
    }
  }, [previewFetcher.data]);

  if (!run) {
    return <p className="py-8 text-center text-sm text-fg-muted">Run not found.</p>;
  }

  return (
    <div>
      <nav className="mb-4 flex items-center gap-1 text-sm text-fg-muted">
        <Link to="/runs" className="text-fg-3 hover:text-fg">Runs</Link>
        {demoMode && (
          <>
            <ChevronRightIcon className="size-3" />
            <Link to={`/workflows/${run.workflow}`} className="text-fg-3 hover:text-fg">
              {run.workflow}
            </Link>
          </>
        )}
        <ChevronRightIcon className="size-3" />
        <span>{run.title}</span>
      </nav>

      <div className="mb-6 flex items-center gap-4">
        <div className="min-w-0 flex-1">
          <h2 className="text-xl font-semibold text-fg">{run.title}</h2>
          <div className="mt-2 flex items-center gap-3 text-sm">
            <span className="flex items-center gap-1.5">
              <span className={`size-2 rounded-full ${run.statusDot}`} />
              <span className={`font-medium ${run.statusText}`}>{run.statusLabel}</span>
            </span>
            <span className="font-mono text-xs text-fg-muted">{run.repo}</span>
            {run.elapsed && (
              <span className="font-mono text-xs text-fg-muted">{run.elapsed}</span>
            )}
          </div>
        </div>
        <button
          type="button"
          title="Open pull request"
          className="flex shrink-0 items-center gap-1.5 rounded-md border border-mint/20 px-3 py-1.5 text-sm font-medium text-mint transition-colors hover:border-mint/50 hover:bg-mint/10 hover:text-fg"
        >
          <svg viewBox="0 0 16 16" fill="currentColor" className="size-3.5" aria-hidden="true">
            <path d="M1.5 3.25a2.25 2.25 0 1 1 3 2.122v5.256a2.251 2.251 0 1 1-1.5 0V5.372A2.25 2.25 0 0 1 1.5 3.25Zm5.677-.177L9.573.677A.25.25 0 0 1 10 .854V2.5h1A2.5 2.5 0 0 1 13.5 5v5.628a2.251 2.251 0 1 1-1.5 0V5a1 1 0 0 0-1-1h-1v1.646a.25.25 0 0 1-.427.177L7.177 3.427a.25.25 0 0 1 0-.354ZM3.75 2.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm0 9.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm8.25.75a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Z" />
          </svg>
          Open PR
        </button>
        {run.sandboxId && (
          <previewFetcher.Form method="post">
            <input type="hidden" name="port" value="3000" />
            <input type="hidden" name="expires_in_secs" value="3600" />
            <button
              type="submit"
              disabled={previewFetcher.state !== "idle"}
              className="flex shrink-0 items-center gap-1.5 rounded-md border border-teal-500/20 px-3 py-1.5 text-sm font-medium text-teal-500 transition-colors hover:border-teal-500/50 hover:bg-teal-500/10 hover:text-fg disabled:opacity-50"
            >
              <svg viewBox="0 0 20 20" fill="currentColor" className="size-3.5" aria-hidden="true">
                <path d="M10 12.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Z" />
                <path fillRule="evenodd" d="M.664 10.59a1.651 1.651 0 0 1 0-1.186A10.004 10.004 0 0 1 10 3c4.257 0 7.893 2.66 9.336 6.41.147.381.146.804 0 1.186A10.004 10.004 0 0 1 10 17c-4.257 0-7.893-2.66-9.336-6.41ZM14 10a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z" clipRule="evenodd" />
              </svg>
              {previewFetcher.state !== "idle" ? "Opening..." : "Preview"}
            </button>
          </previewFetcher.Form>
        )}
        {run.sandboxId && (
          <Menu as="div" className="relative">
            <MenuButton className="flex shrink-0 items-center gap-1.5 rounded-md border border-teal-500/20 px-3 py-1.5 text-sm font-medium text-teal-500 transition-colors hover:border-teal-500/50 hover:bg-teal-500/10 hover:text-fg">
              <svg viewBox="0 0 16 16" fill="currentColor" className="size-3.5" aria-hidden="true">
                <path d="M0 2.75C0 1.784.784 1 1.75 1h12.5c.966 0 1.75.784 1.75 1.75v10.5A1.75 1.75 0 0 1 14.25 15H1.75A1.75 1.75 0 0 1 0 13.25Zm1.75-.25a.25.25 0 0 0-.25.25v10.5c0 .138.112.25.25.25h12.5a.25.25 0 0 0 .25-.25V2.75a.25.25 0 0 0-.25-.25ZM7.25 8a.749.749 0 0 1-.22.53l-2.25 2.25a.749.749 0 1 1-1.06-1.06L5.44 8 3.72 6.28a.749.749 0 1 1 1.06-1.06l2.25 2.25c.141.14.22.331.22.53Zm1.5 1.5h3a.75.75 0 0 1 0 1.5h-3a.75.75 0 0 1 0-1.5Z" />
              </svg>
              Terminal
              <ChevronDownIcon className="size-4" aria-hidden="true" />
            </MenuButton>
            <MenuItems
              transition
              className="absolute right-0 z-10 mt-2 w-48 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:transform data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
            >
              <MenuItem>
                <a
                  href="https://22222-rjyrtjg8gelfyo1p.daytonaproxy01.net/"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="block px-4 py-2 text-sm text-fg-3 data-focus:bg-overlay data-focus:text-fg"
                >
                  Web Terminal
                </a>
              </MenuItem>
              <MenuItem>
                <button
                  type="button"
                  className="block w-full px-4 py-2 text-left text-sm text-fg-3 data-focus:bg-overlay data-focus:text-fg"
                >
                  Connect with SSH
                </button>
              </MenuItem>
            </MenuItems>
          </Menu>
        )}
      </div>

      <div className="border-b border-line">
        <nav className="-mb-px flex gap-6">
          {tabs.map((tab) => {
            const tabPath = `${basePath}${tab.path}`;
            const isActive = tab.name === "Stages"
              ? pathname.startsWith(`${basePath}/stages`)
              : pathname === tabPath;
            return (
              <Link
                key={tab.name}
                to={tabPath}
                className={`border-b-2 pb-3 text-sm font-medium transition-colors ${
                  isActive
                    ? "border-teal-500 text-fg"
                    : "border-transparent text-fg-muted hover:border-line-strong hover:text-fg-3"
                }`}
              >
                {tab.name}
                {tab.count != null && (
                  <span className={`ml-1.5 rounded-full px-1.5 py-0.5 text-xs font-normal tabular-nums ${
                    isActive ? "bg-overlay-strong text-fg-3" : "bg-overlay text-fg-muted"
                  }`}>
                    {tab.count}
                  </span>
                )}
              </Link>
            );
          })}
        </nav>
      </div>

      <div className="mt-6">
        <Outlet />
      </div>
    </div>
  );
}
