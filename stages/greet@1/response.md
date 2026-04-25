Here's a listing of the top-level contents of the repo:

**Directories:**
| Name | Description |
|---|---|
| `.ai` | AI-related config |
| `.cargo` | Cargo configuration |
| `.claude` | Claude config |
| `.config` | General config |
| `.context` | Context files |
| `.fabro` | Fabro workflows and local config |
| `.github` | GitHub Actions CI/CD |
| `apps` | Frontend apps (`fabro-web`, `marketing`) |
| `bin` | Scripts/binaries |
| `docker` | Docker-related files |
| `docs` | Public documentation (API reference, etc.) |
| `docs-internal` | Internal strategy docs |
| `evals` | Evaluations |
| `files-internal` | Internal files (testing strategy, etc.) |
| `installer` | Installation scripts |
| `lib` | Rust crates (`lib/crates/`) and TypeScript packages (`lib/packages/`) |
| `test` | Test fixtures and helpers |

**Key files:**
| Name | Description |
|---|---|
| `AGENTS.md` / `CLAUDE.md` | AI agent guidance (CLAUDE.md symlinks to AGENTS.md) |
| `Cargo.toml` / `Cargo.lock` | Rust workspace manifest |
| `package.json` / `bun.lock` | JS/TS workspace root |
| `Dockerfile` / `Dockerfile.deploy` | Container build definitions |
| `docker-compose.yaml` | Local dev compose |
| `README.md` / `CONTRIBUTING.md` / `LICENSE.md` | Project docs |
| `clippy.toml` / `rustfmt.toml` | Rust linting/formatting config |
| `fly.toml` / `railway.toml` / `render.yaml` | Deployment platform configs |
| `Caddyfile` | Caddy reverse proxy config |
| `install.sh` / `install.md` | Installer entry points (symlinked from `apps/marketing/public/`) |
| `.env.example` | Environment variable template |