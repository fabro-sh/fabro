# Live Provider Workflow Testing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Validate the provider catalog/auth boundary changes against live API keys across model probes, install validation, and real Fabro workflow runs.

**Architecture:** Test the same provider paths at increasing depth: catalog parsing, credential resolution, adapter registration, model probing, run preflight, prompt workflow execution, multi-provider workflows, and agent/tool workflows. Use isolated storage and throwaway API keys so live validation is repeatable and does not leak or mutate local operator state.

**Tech Stack:** Rust, Fabro CLI/server, `cargo nextest`, live OpenAI/Anthropic/Gemini API keys, local sandbox workflow runs.

---

## Scope

This plan validates PR #298 provider/auth changes with live credentials. It intentionally goes beyond `fabro model test` because model tests do not cover workflow graph materialization, provider inference during runs, prompt/agent handlers, preflight, event emission, or per-run provider readiness.

## Prerequisites

- [ ] Create throwaway live keys with low spend limits:
  - `OPENAI_API_KEY`
  - `ANTHROPIC_API_KEY`
  - `GEMINI_API_KEY`
  - Optional OpenAI-compatible provider key such as Kimi, Venice, Zai, LiteLLM, or local Ollama.
- [ ] Save keys in an untracked local file named `.env.live`.
- [ ] Confirm `.env.live` is ignored or otherwise will not be committed:
  ```bash
  git status --short .env.live
  ```
  Expected: no tracked modification or staged secret file.
- [ ] Use isolated Fabro storage for all manual tests:
  ```bash
  export FABRO_STORAGE_DIR="$(mktemp -d)"
  set -a && source .env.live && set +a
  ```
- [ ] Build the current branch:
  ```bash
  cargo build --workspace
  ```
  Expected: build succeeds.

## Phase 1: Automated Live Smoke

- [ ] Run live LLM adapter tests:
  ```bash
  cargo nextest run -p fabro-llm --profile e2e --run-ignored only
  ```
  Expected: live OpenAI, Anthropic, and Gemini tests pass when the matching env vars are present.

- [ ] Run live auth validation tests:
  ```bash
  cargo nextest run -p fabro-auth --profile e2e --run-ignored only
  ```
  Expected: API-key validation succeeds for live configured providers and invalid-key tests still fail cleanly.

- [ ] Run live CLI workflow smoke tests:
  ```bash
  cargo nextest run -p fabro-cli --profile e2e --run-ignored only real_cli
  ```
  Expected: real CLI workflow tests pass for each provider with configured live keys.

## Phase 2: Server And Model Readiness

- [ ] Start the server with isolated storage and live env:
  ```bash
  fabro server start --storage-dir "$FABRO_STORAGE_DIR"
  ```
  Expected: server starts without catalog or credential initialization errors.

- [ ] List configured OpenAI models:
  ```bash
  fabro model list --provider openai
  ```
  Expected: OpenAI models show as configured when `OPENAI_API_KEY` is set.

- [ ] List configured Anthropic models:
  ```bash
  fabro model list --provider anthropic
  ```
  Expected: Anthropic models show as configured when `ANTHROPIC_API_KEY` is set.

- [ ] List configured Gemini models:
  ```bash
  fabro model list --provider gemini
  ```
  Expected: Gemini models show as configured when `GEMINI_API_KEY` or `GOOGLE_API_KEY` is set.

- [ ] Probe OpenAI through the model endpoint:
  ```bash
  fabro model test --provider openai --model gpt-5.2
  ```
  Expected: pass.

- [ ] Probe Anthropic through the model endpoint:
  ```bash
  fabro model test --provider anthropic --model claude-sonnet-4-5
  ```
  Expected: pass.

- [ ] Probe Gemini through the model endpoint:
  ```bash
  fabro model test --provider gemini --model gemini-pro
  ```
  Expected: pass.

- [ ] Run one deeper tool/round-trip model test where cost is acceptable:
  ```bash
  fabro model test --provider anthropic --deep --jobs 1
  ```
  Expected: pass or a provider-specific actionable error if the model rejects the deep probe.

## Phase 3: Install Validation Path

- [ ] Test install validation with OpenAI:
  ```bash
  curl -sS -X POST "http://127.0.0.1:32276/install/llm/test?token=$INSTALL_TOKEN" \
    -H 'content-type: application/json' \
    -d "{\"provider\":\"openai\",\"api_key\":\"$OPENAI_API_KEY\"}"
  ```
  Expected: `{"ok":true}`.

- [ ] Test install validation with Anthropic:
  ```bash
  curl -sS -X POST "http://127.0.0.1:32276/install/llm/test?token=$INSTALL_TOKEN" \
    -H 'content-type: application/json' \
    -d "{\"provider\":\"anthropic\",\"api_key\":\"$ANTHROPIC_API_KEY\"}"
  ```
  Expected: `{"ok":true}`.

- [ ] Test install validation with Gemini:
  ```bash
  curl -sS -X POST "http://127.0.0.1:32276/install/llm/test?token=$INSTALL_TOKEN" \
    -H 'content-type: application/json' \
    -d "{\"provider\":\"gemini\",\"api_key\":\"$GEMINI_API_KEY\"}"
  ```
  Expected: `{"ok":true}`.

- [ ] Test install validation rejects an empty API key:
  ```bash
  curl -sS -X POST "http://127.0.0.1:32276/install/llm/test?token=$INSTALL_TOKEN" \
    -H 'content-type: application/json' \
    -d '{"provider":"openai","api_key":""}'
  ```
  Expected: validation error containing `api_key is required`.

- [ ] Test install validation rejects non-API-key providers:
  ```bash
  curl -sS -X POST "http://127.0.0.1:32276/install/llm/test?token=$INSTALL_TOKEN" \
    -H 'content-type: application/json' \
    -d '{"provider":"ollama","api_key":"unused"}'
  ```
  Expected: validation error explaining the provider does not define an API-key credential path.

## Phase 4: Prompt Workflow Runs

- [ ] Create a temporary prompt workflow:
  ```bash
  WORKFLOW_DIR="$(mktemp -d)"
  cat > "$WORKFLOW_DIR/live-provider-smoke.fabro" <<'EOF'
  digraph LiveProviderSmoke {
    graph [goal="Live provider workflow smoke"]
    start [shape=Mdiamond]
    exit [shape=Msquare]

    smoke [
      shape=tab,
      label="Smoke",
      prompt="Reply with exactly: FABRO_LIVE_OK",
      reasoning_effort="low"
    ]

    start -> smoke -> exit
  }
  EOF
  ```
  Expected: workflow file exists at `$WORKFLOW_DIR/live-provider-smoke.fabro`.

- [ ] Run the prompt workflow with explicit OpenAI provider/model:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --provider openai \
    --model gpt-5.2 \
    --sandbox local \
    --auto-approve
  ```
  Expected: run completes and output contains `FABRO_LIVE_OK`.

- [ ] Run the prompt workflow with explicit Anthropic provider/model:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --provider anthropic \
    --model claude-sonnet-4-5 \
    --sandbox local \
    --auto-approve
  ```
  Expected: run completes and output contains `FABRO_LIVE_OK`.

- [ ] Run the prompt workflow with explicit Gemini provider/model:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --provider gemini \
    --model gemini-pro \
    --sandbox local \
    --auto-approve
  ```
  Expected: run completes and output contains `FABRO_LIVE_OK`.

## Phase 5: Catalog Provider Inference During Runs

- [ ] Run the prompt workflow with OpenAI model only:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --model gpt-5.2 \
    --sandbox local \
    --auto-approve
  ```
  Expected: provider is inferred as OpenAI and the run completes.

- [ ] Run the prompt workflow with Anthropic model only:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --model claude-sonnet-4-5 \
    --sandbox local \
    --auto-approve
  ```
  Expected: provider is inferred as Anthropic and the run completes.

- [ ] Run the prompt workflow with Gemini model only:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --model gemini-pro \
    --sandbox local \
    --auto-approve
  ```
  Expected: provider is inferred as Gemini and the run completes.

## Phase 6: Multi-Provider Workflow Run

- [ ] Create a multi-provider workflow:
  ```bash
  cat > "$WORKFLOW_DIR/multi-provider-live-smoke.fabro" <<'EOF'
  digraph MultiProviderLiveSmoke {
    graph [goal="Verify multiple live providers in one workflow"]
    start [shape=Mdiamond]
    exit [shape=Msquare]

    openai [
      shape=tab,
      provider="openai",
      model="gpt-5.2",
      prompt="Reply exactly: OPENAI_OK"
    ]

    anthropic [
      shape=tab,
      provider="anthropic",
      model="claude-sonnet-4-5",
      prompt="Reply exactly: ANTHROPIC_OK"
    ]

    gemini [
      shape=tab,
      provider="gemini",
      model="gemini-pro",
      prompt="Reply exactly: GEMINI_OK"
    ]

    start -> openai -> anthropic -> gemini -> exit
  }
  EOF
  ```
  Expected: workflow file exists at `$WORKFLOW_DIR/multi-provider-live-smoke.fabro`.

- [ ] Run the multi-provider workflow:
  ```bash
  fabro run "$WORKFLOW_DIR/multi-provider-live-smoke.fabro" \
    --sandbox local \
    --auto-approve
  ```
  Expected: all three nodes complete and output includes `OPENAI_OK`, `ANTHROPIC_OK`, and `GEMINI_OK`.

## Phase 7: Agent/Tool Workflow Runs

- [ ] Create an agent workflow:
  ```bash
  cat > "$WORKFLOW_DIR/live-agent-smoke.fabro" <<'EOF'
  digraph LiveAgentSmoke {
    graph [goal="Verify live LLM agent tool use"]
    start [shape=Mdiamond]
    exit [shape=Msquare]

    agent [
      label="Agent",
      prompt="Create a file named fabro-live-agent-ok.txt containing exactly FABRO_AGENT_OK, then report done."
    ]

    start -> agent -> exit
  }
  EOF
  ```
  Expected: workflow file exists at `$WORKFLOW_DIR/live-agent-smoke.fabro`.

- [ ] Run the agent workflow with Anthropic:
  ```bash
  fabro run "$WORKFLOW_DIR/live-agent-smoke.fabro" \
    --provider anthropic \
    --model claude-sonnet-4-5 \
    --sandbox local \
    --auto-approve
  ```
  Expected: run completes and the run workspace/artifacts contain `fabro-live-agent-ok.txt` with `FABRO_AGENT_OK`.

- [ ] Run the agent workflow with OpenAI:
  ```bash
  fabro run "$WORKFLOW_DIR/live-agent-smoke.fabro" \
    --provider openai \
    --model gpt-5.2 \
    --sandbox local \
    --auto-approve
  ```
  Expected: run completes and the run workspace/artifacts contain `fabro-live-agent-ok.txt` with `FABRO_AGENT_OK`.

## Phase 8: Preflight And Run Agreement

- [ ] Run preflight for the Anthropic prompt workflow:
  ```bash
  fabro preflight "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --provider anthropic \
    --model claude-sonnet-4-5
  ```
  Expected: preflight reports Anthropic as ready.

- [ ] Run the same workflow after preflight:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --provider anthropic \
    --model claude-sonnet-4-5 \
    --sandbox local \
    --auto-approve
  ```
  Expected: run readiness matches preflight and execution completes.

- [ ] Run preflight for the multi-provider workflow:
  ```bash
  fabro preflight "$WORKFLOW_DIR/multi-provider-live-smoke.fabro"
  ```
  Expected: preflight reports all configured providers as ready or gives provider-specific warnings.

## Phase 9: Negative Workflow Cases

- [ ] Remove Anthropic credentials for this shell:
  ```bash
  SAVED_ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-}"
  unset ANTHROPIC_API_KEY
  ```

- [ ] Run the Anthropic prompt workflow without Anthropic credentials:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --provider anthropic \
    --model claude-sonnet-4-5 \
    --sandbox local \
    --auto-approve
  ```
  Expected: run fails with a clear Anthropic auth/configuration error and does not report unrelated providers as broken.

- [ ] Restore Anthropic credentials:
  ```bash
  export ANTHROPIC_API_KEY="$SAVED_ANTHROPIC_API_KEY"
  ```

- [ ] Point OpenAI at an unreachable base URL:
  ```bash
  SAVED_OPENAI_BASE_URL="${OPENAI_BASE_URL:-}"
  export OPENAI_BASE_URL="http://127.0.0.1:9/v1"
  ```

- [ ] Run the OpenAI prompt workflow with the unreachable base URL:
  ```bash
  fabro run "$WORKFLOW_DIR/live-provider-smoke.fabro" \
    --provider openai \
    --model gpt-5.2 \
    --sandbox local \
    --auto-approve
  ```
  Expected: run fails with an OpenAI-specific connection or provider readiness error.

- [ ] Restore OpenAI base URL:
  ```bash
  if [ -n "$SAVED_OPENAI_BASE_URL" ]; then
    export OPENAI_BASE_URL="$SAVED_OPENAI_BASE_URL"
  else
    unset OPENAI_BASE_URL
  fi
  ```

- [ ] Validate malformed custom header catalog override:
  ```bash
  BAD_CATALOG="$(mktemp)"
  cat > "$BAD_CATALOG" <<'EOF'
  [providers.bad]
  display_name = "Bad"
  adapter = "openai"
  agent_profile = "openai"

  [providers.bad.auth]
  type = "api_key"
  credentials = ["env:BAD_API_KEY"]
  header = { custom = "bad header" }
  EOF
  ```
  Expected: a catalog load using this override fails with `custom header name must be a valid HTTP header name`.

- [ ] Validate OpenAI-compatible provider without `base_url`:
  ```bash
  MISSING_BASE_URL_CATALOG="$(mktemp)"
  cat > "$MISSING_BASE_URL_CATALOG" <<'EOF'
  [providers.no_base]
  display_name = "No Base"
  adapter = "openai_compatible"
  agent_profile = "openai"

  [providers.no_base.auth]
  type = "api_key"
  credentials = ["env:NO_BASE_API_KEY"]
  header = "bearer"

  [models."no-base-model"]
  provider = "no_base"
  display_name = "No Base Model"
  family = "test"
  default = true

  [models."no-base-model".limits]
  context_window = 1000

  [models."no-base-model".features]
  tools = false
  vision = false
  reasoning = false
  EOF
  ```
  Expected: catalog parsing succeeds, but client/provider registration reports that `openai_compatible` requires `base_url`.

## Phase 10: Optional Provider Coverage

- [ ] Test one configured OpenAI-compatible API-key provider such as Kimi, Venice, Zai, MiniMax, Inception, or LiteLLM.
- [ ] Test local Ollama/no-auth provider if a local model is available and catalog overrides enable `ollama`.
- [ ] Test one header-only proxy provider if an operator has a real proxy service available.

## Phase 11: Log And Secret Review

- [ ] Inspect terminal output from all runs for leaked secret values.
- [ ] Inspect server logs for leaked secret values.
- [ ] Inspect failed run events for leaked secret values:
  ```bash
  fabro runs list
  fabro events <run-id>
  ```
  Expected: provider names and error summaries are visible, but raw API keys are not.

## Sign-Off Criteria

- [ ] Automated live tests pass for the configured providers.
- [ ] `fabro model test` passes for OpenAI, Anthropic, and Gemini.
- [ ] Prompt workflow runs pass for OpenAI, Anthropic, and Gemini.
- [ ] Catalog provider inference works during `fabro run`.
- [ ] One multi-provider workflow run completes.
- [ ] At least one agent/tool workflow run completes.
- [ ] Preflight and run readiness agree.
- [ ] Negative credential/base-URL/config cases fail with provider-specific, actionable errors.
- [ ] No live secrets appear in logs, terminal output, JSON responses, or run events.
