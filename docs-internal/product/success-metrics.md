# Fabro Success Metrics

For product work, measure trust and useful outcomes, not just command execution.

## Signals available now

- CLI telemetry for command success and failure
- per-run event streams in `progress.jsonl`
- run artifacts such as `checkpoint.json`, `conclusion.json`, and verification data
- `retro.json` when retros are enabled
- API and web data for runs, workflows, usage, verifications, and insights

## Metrics that matter most

- successful outcomes per workflow type
- cost and duration per successful outcome
- verification pass rate
- retry, loop, and human-intervention rate
- resume, rewind, and fork usage

## Current gaps

- command success is not the same as product success
- install, activation, and retention metrics are weak
- some aggregate usage is server-local and resets on restart
- cross-run comparison is still immature
