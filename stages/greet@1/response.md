Here's a breakdown of the top-level contents of the repo:

### Configuration & Metadata
| Entry | Description |
|---|---|
| `AGENTS.md` / `CLAUDE.md` | Agent guidance docs (`CLAUDE.md` is a symlink to `AGENTS.md`) |
| `CONTRIBUTING.md` | Contribution guidelines |
| `LICENSE.md` | License |
| `README.md` | Project readme |
| `Cargo.toml` / `Cargo.lock` | Rust workspace manifest and lockfile |
| `package.json` / `bun.lock` | Root JS package config and Bun lockfile |
| `rustfmt.toml` / `clippy.toml` | Rust formatting and linting config |
| `.env.example` | Example environment variables |
| `.gitignore` / `.gitattributes` | Git config |

### Docker & Deployment
| Entry | Description |
|---|---|
| `Dockerfile` / `Dockerfile.deploy` | Container build files |
| `docker-compose.yaml` / `docker-compose.prod.yaml` | Docker Compose configs |
| `Caddyfile` | Caddy reverse proxy config |
| `fly.toml` / `railway.toml` / `render.yaml` | Cloud platform deployment configs |

### Source Directories
| Entry | Description |
|---|---|
| `lib/` | Rust crates (core libraries) |
| `apps/` | Frontend apps (`fabro-web`, `marketing`) |
| `bin/` | Scripts/binaries |
| `installer/` | Installer tooling |
| `test/` | Test fixtures and helpers |
| `evals/` | Evaluation suite |
| `docker/` | Additional Docker assets |

### Documentation
| Entry | Description |
|---|---|
| `docs/` | Public-facing docs (API reference, etc.) |
| `docs-internal/` | Internal strategy docs |
| `files-internal/` | Internal reference files |

### Hidden / Tool Config
| Entry | Description |
|---|---|
| `.fabro/` | Fabro workflow definitions |
| `.github/` | GitHub Actions CI/CD |
| `.cargo/` | Cargo config |
| `.claude/` | Claude agent config |
| `.ai/` / `.context/` / `.config/` | AI and tool context |
| `install.sh` / `install.md` | Symlinks into `apps/marketing/public/` |