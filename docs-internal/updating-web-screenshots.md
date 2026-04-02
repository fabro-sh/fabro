# Updating Web UI Screenshots

Screenshots of the Fabro web UI are embedded in the public docs. This guide covers how to retake them when the UI changes.

## Prerequisites

- Docker installed
- Chrome running with DevTools MCP or similar screenshot tool
- The `fabro` Docker image already built (`docker compose -f docker/docker-compose.yaml build`)

## Boot the demo environment

```bash
docker compose -f docker/docker-compose.yaml up api web -d
```

Wait ~10 seconds for both services to be ready, then verify:

```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/runs
# Should return 200
```

The web container runs in demo mode (`FABRO_DEMO=1`), which returns synthetic data from the API.

## Applying temporary UI changes for screenshots

The Docker image bakes in the fabro-web source at build time. To make temporary changes (like hiding nav items), edit files inside the running container using `sed`:

```bash
# Example: hide Start and Settings nav items
docker exec docker-web-1 sed -i '/{.*"Start".*}/d; /{.*"Settings".*}/d' \
  /app/apps/fabro-web/app/layouts/app-shell.tsx
```

**Do not use `docker cp`** to replace source files — it breaks Vite's module resolution and causes 500 errors. Use `sed -i` inside the container instead, which preserves the file inode and lets HMR work correctly.

`docker cp` works fine for static assets in `public/` (logos, images), just not for source files that Vite processes.

After `sed` edits, wait a few seconds for HMR to rebuild, then verify pages still return 200 before taking screenshots.

## Updating logos

The logos are static files in `apps/fabro-web/public/`. The Docker compose file mounts this directory as a volume:

```yaml
volumes:
  - ../apps/fabro-web/public:/app/apps/fabro-web/public
```

So changes to `apps/fabro-web/public/logotype.svg` (dark theme) and `apps/fabro-web/public/logotype-light.svg` (light theme) are picked up immediately by the running container. The source-of-truth logos are in `docs/logo/dark.svg` and `docs/logo/light.svg`.

## Browser setup

Set the browser viewport to **1200x800**. This width:
- Fits the full nav bar without overlap
- Provides a good aspect ratio for embedding in docs
- Shows enough content in kanban boards and tables

If the nav bar is too crowded at this width, hide low-priority items (Start, Settings) using the `sed` technique above.

## Taking screenshots

Screenshots live in `docs/images/web/`. Each screenshot maps to a specific URL:

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
- **After `sed` edits** — HMR needs a few seconds to rebuild. The first navigation after an edit may hit a transient error; retry once.

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
docker compose -f docker/docker-compose.yaml down
```

If you also started the docs server:

```bash
docker stop mintlify-dev
```
