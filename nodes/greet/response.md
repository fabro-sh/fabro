Here's a breakdown of the top-level contents of the Fabro repository:

### Configuration & Metadata
| Entry | Description |
|---|---|
| `.env.example` | Template for environment variables (API keys, credentials) |
| `.gitattributes` | Git file attribute settings |
| `.gitignore` | Git ignore rules |
| `Cargo.lock` | Rust dependency lockfile |
| `Cargo.toml` | Rust workspace manifest |
| `bun.lock` | Bun (JS) dependency lockfile |
| `package.json` | Root JS package config |
| `clippy.toml` | Rust linter (Clippy) config |
| `rustfmt.toml` | Rust formatter config |

### Documentation
| Entry | Description |
|---|---|
| `README.md` | Project overview |
| `AGENTS.md` | Guidance for AI agents working in this repo |
| `CLAUDE.md` | Symlink → `AGENTS.md` (Claude-specific alias) |
| `CONTRIBUTING.md` | Contributor guidelines |
| `LICENSE.md` | License |
| `docs/` | Public-facing documentation (API reference, guides) |
| `docs-internal/` | Internal strategy docs (logging, events, testing) |
| `files-internal/` | Internal reference files |

### Source Code
| Entry | Description |
|---|---|
| `lib/` | Shared Rust crates (`fabro-workflow`, `fabro-llm`, etc.) and TypeScript packages |
| `apps/` | Frontend apps — `fabro-web` (React UI) and `marketing` (Astro site) |
| `bin/` | Binary entry points |
| `installer/` | Installation scripts/tooling |
| `scripts/` | Dev/ops helper scripts |

### Runtime & Dev Tooling
| Entry | Description |
|---|---|
| `.fabro/` | Fabro project config & workflow definitions for this repo itself |
| `.ai/` | AI-related config/context |
| `.cargo/` | Cargo toolchain config |
| `.claude/` | Claude-specific config |
| `.config/` | General tool config |
| `.github/` | GitHub Actions workflows and PR templates |
| `docker/` | Dockerfiles for containerized execution |
| `evals/` | Evaluation harnesses |
| `test/` | Integration / E2E test fixtures |

### Symlinks
| Entry | Points To |
|---|---|
| `install.sh` | `apps/marketing/public/install.sh` |
| `install.md` | `apps/marketing/public/install.md` |

This is a **monorepo** combining Rust backend crates, a React frontend, a marketing site, and all the tooling for Fabro — an AI-powered workflow orchestration platform. The core engine lives in `lib/crates/`, with workflows defined as Graphviz graphs.