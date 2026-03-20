# Fabro Technical Requirements

This note captures stable constraints that product changes should respect.

## Core constraints

- Fabro ships primarily as a single Rust binary with CLI and server modes.
- Workflows are defined in Graphviz DOT and should remain reviewable as source files.
- The workflow engine must support loops, branching, parallel stages, commands, agent stages, and human gates.
- Model routing is per-stage and provider-agnostic through stylesheets and config.
- Execution happens through sandbox providers rather than assuming direct host access.
- Git checkpointing is central to resume, rewind, fork, and auditability.
- Runs produce structured artifacts such as `progress.jsonl`, `live.json`, `checkpoint.json`, and optional `retro.json`.
- The HTTP API is OpenAPI-based, and the web app depends on that contract.

## Operational constraints

- Documented targets are macOS arm64, Linux x86_64, and Linux arm64.
- Git is required for checkpointing-related workflows.
- Docker, Graphviz, and SSH are optional system dependencies depending on the features in use.

## Design bias

Prefer changes that improve determinism, observability, resumability, and safe unattended execution. Avoid features that only make sense as IDE autocomplete or a chat-first REPL.
