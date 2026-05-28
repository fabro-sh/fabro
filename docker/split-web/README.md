# Split Web Compose PoC

This Compose stack proves that Fabro can serve the React SPA from a separate
static process while the Rust server remains the API and browser-auth origin.

Request ownership:

- `/api/*` -> `fabro-api:32276`
- `/auth/*` -> `fabro-api:32276`
- `/health` -> `fabro-api:32276`
- everything else -> `fabro-web:80`

The Rust server still contains bundled SPA assets. In this PoC they are simply
not reachable through the `edge` service for normal web paths.

The edge proxy adds `X-Fabro-PoC-Upstream` to responses so manual checks can
confirm which service handled a request.

## Run

Build the local Fabro image from the current tree:

```sh
cargo dev docker-build --tag fabro-sh/fabro:split-web-poc
```

Set local auth secrets:

```sh
export SESSION_SECRET="$(openssl rand -hex 32)"
export FABRO_DEV_TOKEN="fabro_dev_$(openssl rand -hex 32)"
```

Start the split stack:

```sh
docker compose -f docker-compose.split-web.yaml up
```

Open http://localhost:8080.

Use `SPLIT_WEB_PORT` to expose a different local port, or `FABRO_IMAGE` to use
a different API image.

## Validate

```sh
curl -i http://localhost:8080/health
curl -i http://localhost:8080/api/v1/health
curl -I http://localhost:8080/runs
curl -I http://localhost:8080/assets/app.css

curl -c /tmp/fabro.cookies \
  -H "content-type: application/json" \
  -d "{\"token\":\"$FABRO_DEV_TOKEN\"}" \
  http://localhost:8080/auth/login/dev-token

curl -b /tmp/fabro.cookies http://localhost:8080/api/v1/auth/me
```

Expected results:

- `/runs` and `/assets/*` are served by the static `fabro-web` container.
- `/api/*`, `/auth/*`, and `/health` are served by the Rust `fabro-api`
  container through the same browser origin.
- Dev-token login sets a same-origin session cookie, and
  `/api/v1/auth/me` accepts it.
