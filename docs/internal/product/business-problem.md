# The Problem Fabro Solves

Fabro exists for teams that have outgrown the prompt-act-review loop used by most AI coding tools.

## Why that loop breaks

Interactive agent sessions work for small tasks, but they do not define a repeatable software process. In practice this creates four recurring problems:

- The engineer must supervise the agent instead of defining the process once and reusing it.
- There is no reliable gate between "the agent stopped" and "the work is ready."
- Model choice is hard to control per step, so cost and quality drift.
- Sessions are hard to resume, audit, and improve over time.

## What Fabro adds

Fabro treats the process itself as code:

- Workflow graphs define stages, branching, loops, parallelism, and human gates.
- Model stylesheets route different stages to different models and providers.
- Sandboxes and Git checkpoints make runs isolated, resumable, and inspectable.
- Event logs, verifications, and retros create a feedback loop after execution.

## Product direction

Fabro should optimize for trust in long-running workflows, not for chat UX. The product is most useful when expert engineers can design, run, inspect, and improve processes with minimal supervision.
