Read the feature blueprint matching $goal under files-internal/architecture/features/.
Read the foundation blueprints under files-internal/architecture/foundation-blueprints/.
Read the acceptance criteria from the matching doc under files-internal/product/features/.
Read the relevant existing source code.

Decomposition strategy: $context.human.gate.label

Decompose the blueprint into ordered implementation steps using the chosen strategy:

- **Layer-by-layer:** Group steps by technical layer — database migrations, then data models, then API endpoints, then UI components, then tests for each layer.
- **Feature slice:** Group steps by user-facing capability — each step delivers a vertical slice from database through UI for one piece of functionality.
- **Embarrassingly parallel:** Identify steps with no dependencies on each other and group them for concurrent implementation. Mark dependency ordering explicitly.
- **Sequential / linear:** One step per logical change, strictly ordered, each building on the previous.

For each step, specify the exact files to create or modify and what changes to make.

Do not plan work for anything listed in the blueprint's Out of Scope section. Every step must trace to an acceptance criterion.
