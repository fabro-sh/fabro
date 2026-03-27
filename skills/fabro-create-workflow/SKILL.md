---
name: fabro-create-workflow
description: Create Fabro workflow DOT graphs and TOML run configurations from natural language requirements. Use when the user wants to create a new workflow, build a pipeline, design a multi-step agent process, or write a .fabro or .toml file for Fabro. Covers topology selection, node types, model assignment, edge routing, and run configuration.
---

# Fabro Create Workflow

Turn requirements into a runnable Fabro workflow: a `.fabro` graph file defining the pipeline structure and an optional `.toml` run configuration.

## Workflow

### Step 1: Fetch Current Model Catalog

Run `fabro model list` to get available models and providers. Never guess model IDs or provider names -- they change frequently.

### Step 2: Understand Requirements

Clarify what the workflow should accomplish:
- What is the end goal?
- What tools or languages are involved?
- Does it need human approval gates?
- Should multiple models or providers be used?
- Is parallelism needed (e.g., multi-perspective review, ensemble)?
- Does it need a verify/fix loop?
- What sandbox environment is appropriate (local, docker, daytona)?

### Step 3: Choose Topology

Pick the simplest topology that satisfies the requirements. See `references/example-workflows.md` for complete examples of each pattern.

| Pattern | When to use |
|---|---|
| **Linear** | Simple sequential steps, no branching |
| **Command-then-analyze** | Shell output feeds into LLM analysis |
| **Implement-test-fix loop** | Code generation with validation cycle |
| **Human approval gate** | Needs human review before proceeding |
| **Plan-approve-implement** | Complex changes needing upfront planning |
| **Parallel fan-out** | Independent analyses merged into synthesis |
| **Multi-model ensemble** | Multiple providers give independent opinions |
| **Production pipeline** | Toolchain checks + implement + verify + fixup loops |

Combine patterns as needed. For example, a production pipeline might include a human gate after planning and a parallel fan-out for review.

### Step 4: Write the DOT Graph

See `references/dot-language.md` for the full language reference.

**Required elements:**
1. `digraph Name { ... }` wrapper
2. `graph [goal="..."]` attribute
3. `rankdir=LR` (preferred for readability)
4. Exactly one `start [shape=Mdiamond, label="Start"]`
5. Exactly one `exit [shape=Msquare, label="Exit"]`

**Choose node shapes by purpose:**
- `box` (default) -- agent with tools (implement, fix, write code)
- `tab` -- single LLM call without tools (analyze, plan, review, synthesize)
- `parallelogram` -- shell command (build, test, lint)
- `diamond` -- conditional routing (no prompt, only conditions on edges)
- `hexagon` -- human decision gate
- `component` -- parallel fan-out
- `tripleoctagon` -- merge parallel results

**Prompt guidelines:**
- Be specific and actionable in prompts
- Tell the agent exactly what to do, what files to create/modify, what output to produce
- Use `shape=tab` for nodes that only need to think, not act
- Use `prompt="@path/to/file.md"` for long prompts (path relative to DOT file)
- Set `reasoning_effort="low"` on simple analysis or summary nodes

**Edge routing:**
- Use `condition="outcome=success"` and unconditional fallback for check gates
- Diamond nodes must have multiple outgoing edges with conditions
- Use `max_visits` on fix/retry nodes to prevent infinite loops (typically 3)
- Use `goal_gate=true` on verification nodes that must pass for the workflow to succeed
- Set `retry_target` on goal gates to specify where to jump on failure

### Step 5: Assign Models via Stylesheet

Use `model_stylesheet` for model assignment rather than per-node attributes:

```dot
graph [model_stylesheet="
    *        { model: claude-sonnet-4-6;}
    .coding  { model: claude-opus-4-6;}
    .review  { model: gemini-3.1-pro-preview;}
"]
```

- Use `*` for the default model (usually a fast/cheap model)
- Use `.class` selectors for role-based assignment (`.coding`, `.review`, `.verify`)
- Use `#nodeid` selectors for specific node overrides
- Assign `class="coding"` etc. on nodes to match stylesheet rules
- **Critical:** Use semicolons between properties in stylesheet rules

**Model selection heuristics:**
- Fast/cheap models for simple analysis, summaries, routing: `claude-haiku-4-5`, `gemini-3-flash-preview`, `gpt-5-mini`
- Strong models for coding, complex reasoning: `claude-opus-4-6`, `claude-sonnet-4-6`, `gpt-5.4`
- Use `reasoning_effort="high"` for complex coding tasks
- For ensembles, pick models from different providers for diversity

### Step 6: Write the TOML Run Configuration (if needed)

See `references/run-configuration.md` for the full reference.

A TOML file is optional for simple workflows (you can run `fabro run workflow.fabro` directly). Create one when you need:
- Sandbox configuration (provider, environment variables)
- Setup commands (install dependencies)
- Variable definitions
- LLM fallbacks
- Hooks
- Asset collection

Minimal TOML:

```toml
version = 1
graph = "workflow.fabro"
```

Common additions:

```toml
[sandbox]
provider = "local"

[sandbox.local]
worktree_mode = "always"

[sandbox.env]
NODE_ENV = "test"
```

### Step 7: Validate

Run `fabro preflight workflow.toml` (or `fabro preflight workflow.fabro`) to validate without executing.

If validation fails, fix the reported errors and re-validate.

## Guardrails

- **Diamond nodes route only.** Never put a `prompt` on a `diamond` node -- it only evaluates edge conditions.
- **All nodes reachable from start.** No orphan nodes.
- **No edges into start or out of exit.**
- **LLM nodes need prompts.** Every `box` and `tab` node must have a `prompt` attribute.
- **Conditional nodes need multiple outgoing edges** with `condition` attributes.
- **Prevent infinite loops.** Use `max_visits` on retry/fix nodes. Typical value: 3.
- **Use `goal_gate=true`** on critical verification steps that must succeed.
- **Use `outcome=success` conditions** on edges leaving command/conditional nodes.
- **Model IDs must match the catalog.** Always run `fabro model list` first.
- **Semicolons in stylesheets.** Properties must be separated by semicolons.

## File Organization

Place workflow files together in a directory:

```
my-workflow/
  workflow.fabro        # the graph
  workflow.toml         # run configuration (optional)
  prompts/              # external prompt files (optional)
    implement.md
    review.md
```

## Running Workflows

```bash
fabro run workflow.fabro                  # run graph directly
fabro run workflow.toml                   # run with TOML config
fabro run workflow.toml --dry-run         # simulated LLM backend
fabro run workflow.toml --no-retro        # skip retro (faster for testing)
fabro run workflow.toml --auto-approve    # auto-approve human gates
fabro run workflow.toml --model claude-opus-4-6  # override model
fabro run workflow.toml --sandbox local   # override sandbox
fabro validate workflow.fabro            # validate only
```

## References

- `references/dot-language.md` -- Complete DOT language reference (node types, attributes, edges, conditions, stylesheets, fidelity, variables)
- `references/run-configuration.md` -- Complete TOML run configuration reference (sandbox, setup, hooks, LLM, vars, assets)
- `references/example-workflows.md` -- 9 complete example workflows from simple to production-grade

{{user_input}}
