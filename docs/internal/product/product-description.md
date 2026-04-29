# Fabro Product Description

Fabro is an open-source workflow orchestration platform for AI coding agents. Instead of treating software work as a chat session, Fabro lets you define the process as a Graphviz workflow and run it repeatedly.

## What a workflow can contain

A workflow can combine:

- agent stages
- prompt stages
- shell commands
- conditionals
- human gates
- parallel branches

## What supports execution

- model stylesheets choose models and providers per stage
- sandboxes isolate execution from the host
- Git checkpoints make runs resumable and auditable
- event logs, verifications, and optional retros help teams inspect outcomes

## Interfaces

Fabro is CLI-first, with an API server and React web app for longer-running and shared workflows.
