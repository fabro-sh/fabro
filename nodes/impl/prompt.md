Goal: Full-stack scenario: command, agent, human gate, and goal gate verification

## Completed stages
- **setup**: success
  - Script: `mkdir -p /tmp/scenario_full && echo ready > /tmp/scenario_full/flag.txt`
  - Stdout: (empty)
  - Stderr: (empty)
- **plan**: success
  - Model: claude-haiku-4-5, 2.3k tokens in / 355 out
- **approve**: success

## Context
- human.gate.label: [A] Approve
- human.gate.selected: A


Create the file /tmp/scenario_full/result.txt containing exactly the word PASS on the first line.