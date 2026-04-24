Here's a breakdown of the top-level contents of the Fabro repo:

### 📁 Directories
| Directory | Purpose |
|-----------|---------|
| `.ai` | AI-related config/context |
| `.cargo` | Cargo configuration |
| `.claude` | Claude-specific config |
| `.config` | General config files |
| `.context` | Context files |
| `.fabro` | Fabro project config (workflows, etc.) |
| `.github` | GitHub Actions CI/CD workflows |
| `apps` | Application frontends (fabro-web, marketing site) |
| `bin` | Developer scripts and binaries |
| `docker` | Docker-related files |
| `docs` | Public documentation (OpenAPI spec, Mintlify) |
| `docs-internal` | Internal strategy/architecture docs |
| `evals` | Evaluation suites |
| `files-internal` | Internal reference files (e.g. testing strategy) |
| `installer` | Installer tooling |
| `lib` | Core Rust crates (`lib/crates/`) and TypeScript packages (`lib/packages/`) |
| `scripts` | Build and utility scripts |
| `test` | Test fixtures and shared test infrastructure |

### 📄 Key Files
| File | Purpose |
|------|---------|
| `AGENTS.md` / `CLAUDE.md` | Agent guidance (CLAUDE.md symlinks to AGENTS.md) |
| `Cargo.toml` / `Cargo.lock` | Rust workspace manifest and lockfile |
| `Dockerfile` / `Dockerfile.deploy` | Container build definitions |
| `README.md` | Project readme |
| `CONTRIBUTING.md` | Contribution guidelines |
| `clippy.toml` / `rustfmt.toml` | Rust linter and formatter config |
| `docker-compose.yaml` | Local Docker Compose setup |
| `install.sh` / `install.md` | Installer (symlinks into `apps/marketing/public/`) |
| `bun.lock` / `package.json` | Bun/Node package lockfile and root manifest |
| `fly.toml` / `railway.toml` / `render.yaml` | Cloud deployment configs |
| `Caddyfile` | Caddy reverse proxy config |

This is a monorepo combining a Rust workspace (the core Fabro server/CLI/engine) with TypeScript apps (React web UI, marketing site) and supporting infrastructure.