# Updating Web UI Screenshots

Screenshots of the Fabro web UI are embedded in the public docs. This guide covers how to retake them when the UI changes.

> **Known gaps** after the server absorbed the web UI (April 2026):
> - **Demo mode** is now per-request via the `X-Fabro-Demo: 1` header, not a container env var. The browser won't send it by default — you'll need a browser extension (e.g. ModHeader) or a sidecar proxy to inject the header. Needs a follow-up to land a server-level toggle.
> - **HMR trick is gone.** Web assets are baked into the Rust binary (`fabro-spa/assets`), so the `sed` technique for hiding nav items no longer works. Either commit a temporary nav change to a local branch and rebuild, or script visibility changes in the browser devtools.

## Prerequisites

- Docker installed
- Chrome running with DevTools MCP or similar screenshot tool
- Header-injection tool (browser extension or proxy) to send `X-Fabro-Demo: 1`
- The `fabro` Docker image built locally: `cargo dev docker-build`

## Boot the demo environment

```bash
docker compose up -d
```

Wait ~5 seconds for the server to be ready, then verify (sending the demo header):

```bash
curl -s -H "X-Fabro-Demo: 1" -o /dev/null -w "%{http_code}" http://localhost/runs
# Should return 200
```

## Applying temporary UI changes for screenshots

With the SPA baked into the binary, there's no in-container edit path. Commit the nav change on a local branch, rerun `cargo dev docker-build`, then `docker compose up -d --force-recreate`.

## Updating logos

Logos live at `apps/fabro-web/public/logotype.svg` (dark) and `apps/fabro-web/public/logotype-light.svg` (light). These are bundled into the SPA at build time — rebuilding the image picks up the changes. The source-of-truth logos are in `docs/public/logo/dark.svg` and `docs/public/logo/light.svg`.

## Browser setup

Set the browser viewport to **1200x800**. This width:
- Fits the full nav bar without overlap
- Provides a good aspect ratio for embedding in docs
- Shows enough content in kanban boards and tables

If the nav bar is too crowded at this width, hide low-priority items (Start, Settings) using the `sed` technique above.

## Taking screenshots

Screenshots live in `docs/public/images/web/`. Each screenshot maps to a specific URL (served from `http://localhost/` with the `X-Fabro-Demo: 1` header):

| File | URL |
|---|---|
| `workflows-list.png` | `/workflows` |
| `workflow-detail.png` | `/workflows/fix_build` |
| `workflow-diagram.png` | `/workflows/fix_build/diagram` |
| `workflow-runs.png` | `/workflows/fix_build/runs` |
| `runs-board.png` | `/runs` |
| `run-overview.png` | `/runs/run-1` |
| `run-stages.png` | `/runs/run-1/stages/detect-drift` |
| `run-files-changed.png` | `/runs/run-1/compare` |
| `run-retro.png` | `/runs/run-1/retro` |
| `run-usage.png` | `/runs/run-1/usage` |
| `retros-list.png` | `/retros` |

### Verification checklist

**Verify every screenshot after taking it.** Open the saved PNG and check:

1. **No 500 errors** — the most common failure mode. The demo API or HMR can transiently break. If you see a 500 error page, wait a few seconds and retake.
2. **Correct logo** — should say "Fabro", not "Arc".
3. **No nav overlap** — the rightmost nav item should not overlap the theme toggle or user avatar.
4. **Content fully loaded** — watch for "Loading diagram..." or spinner states. For the run overview page, wait 2-3 seconds after navigation for the workflow graph to render.
5. **Dark theme** — all screenshots should use the dark theme (default). If you accidentally toggled to light theme, toggle back before continuing.
6. **Correct page** — verify the active tab/breadcrumb matches the expected page.

### Pages that need extra wait time

- **Run overview** (`/runs/run-1`) — the workflow graph diagram takes 2-3 seconds to render after the page loads. Wait before screenshotting.

## Where screenshots are used in docs

Each screenshot is wrapped in a `<Frame caption="...">` component. To find all usages:

```bash
grep -r "images/web/" docs/ --include="*.mdx"
```

Current placements:

| Screenshot | Doc page |
|---|---|
| `runs-board.png` | `core-concepts/how-fabro-works.mdx` |
| `run-overview.png` | `core-concepts/how-fabro-works.mdx` |
| `workflows-list.png` | `core-concepts/workflows.mdx` |
| `workflow-detail.png` | `core-concepts/workflows.mdx` |
| `workflow-diagram.png` | `core-concepts/workflows.mdx` |
| `workflow-runs.png` | `core-concepts/workflows.mdx` |
| `run-stages.png` | `execution/observability.mdx` |
| `run-usage.png` | `execution/observability.mdx` |
| `retros-list.png` | `execution/retros.mdx` |
| `run-retro.png` | `execution/retros.mdx` |
| `run-files-changed.png` | `human-tools/steering.mdx` |

## Cleanup

```bash
docker compose down
```

If you also started the docs server:

```bash
docker stop mintlify-dev
```
