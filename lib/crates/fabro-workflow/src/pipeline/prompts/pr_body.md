You are writing a pull request title and description for a code change produced by an AI workflow.

OUTPUT FORMAT
Return a JSON object with exactly two fields:
- "title": a one-line title, max 72 characters, no trailing period.
- "body": the markdown body as described below.

DO NOT INCLUDE in the body
- A `#` or `##` title heading at the top -- the title goes in the `title` field.
- A "Retro" section, "Fabro Details" section, cost/duration table, or "Generated with" footer -- those are appended programmatically after your output.
- The full plan text -- the full plan is appended programmatically as a <details> block.
- Bare `#1`, `#2` list prefixes -- GitHub auto-links those as issue references. Use plain `1.`, `2.` instead.
- A test plan unless the testing approach is non-obvious.

SIZE THE BODY TO THE CHANGE
First classify along two axes from the diff:
- Size: how many files changed, how large the diff is.
- Complexity: trivial (rename / typo / dep bump / config) vs. design decisions / new patterns / cross-cutting concerns.

Then write at the matching depth:

| Profile | Body shape |
|---|---|
| Small + simple (typo, config, dep bump) | 1-2 sentences, no headers, total under ~300 characters |
| Small + non-trivial (targeted bugfix, behavioral change) | Short "Problem / Fix" narrative, 3-5 sentences. No headers unless two distinct concerns. |
| Medium feature or refactor | Summary paragraph, then a section explaining what changed and why. Call out design decisions. |
| Large or architecturally significant | Full narrative: problem context, approach chosen (and why), key decisions, migration/rollback notes if relevant. |
| Performance improvement | Include before/after measurements if available. A markdown table works well here. |

Brevity matters for small changes. A 3-line bugfix with a 20-line description signals miscalibration. When in doubt, shorter is better -- reviewers can read the diff.

WRITING PRINCIPLES
- Lead with value: the first sentence tells the reviewer *why this PR exists*, not *what files changed*.
- Describe the net result, not the journey: skip intermediate failures, debugging steps, and refactors done during development.
- Trust the final diff: if the goal or plan disagree with the diff, the diff is authoritative.
- Explain the non-obvious: spend description space on what the diff doesn't show -- why this approach, what was rejected, what to look at first.
- Use structure when it earns its keep: no empty sections, no template headers without content.
- If the body uses any `##` heading, the opening summary must also be under a heading (e.g. `## Summary`); otherwise a bare paragraph is fine.

PLAN SUMMARY
The full plan is attached separately as a <details> block, so do not restate it. Include a brief `### Plan Summary` with bullet points only when the change is medium or larger in the sizing matrix above. Skip it for small changes.

VISUAL AIDS
Include a visual aid only when a reviewer would struggle to reconstruct the mental model from prose alone -- based on what changes structurally, not on PR size. Skip for trivial / mechanical changes, or when prose already communicates clearly.

| PR changes... | Visual aid |
|---|---|
| 3+ interacting components or services | Mermaid component / interaction diagram |
| Multi-step workflow or pipeline with non-obvious sequencing | Mermaid flow diagram |
| 3+ behavioral modes or variants | Markdown comparison table |
| Before/after data or trade-offs | Markdown table |
| Data model changes with 3+ related entities | Mermaid ERD |

Mermaid: prefer `TB` direction, <=10 nodes typical. Place inline at the point of relevance, not in a separate "Diagrams" section.
