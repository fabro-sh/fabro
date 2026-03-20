# Fabro Current State

This snapshot is intentionally brief and omits volatile counts, benchmarks, and release metrics.

## Public product shape

Fabro currently presents as:

- a CLI for defining and running workflows
- an API server via `fabro serve`
- a React web app for monitoring runs and inspecting workflows
- a docs site and example workflows

## Core capabilities documented today

- Graphviz DOT workflows with agent, prompt, command, conditional, human, and parallel stages
- CSS-like model stylesheets for per-stage model routing
- sandboxed execution across local, Docker, Daytona, SSH, and other providers
- Git checkpointing for resume, rewind, fork, and auditability
- structured run artifacts and event streams for observability
- verifications and insights in the broader product surface
- retrospectives, documented as experimental

## Current positioning

Fabro is positioned as an open-source, self-hosted workflow orchestration layer for expert engineers. It is not positioned as an IDE plugin, autocomplete tool, or chat-first coding assistant.
