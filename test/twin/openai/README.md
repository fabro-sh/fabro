# twin-openai

Async Rust fake OpenAI-compatible server for local black-box testing.

## Endpoints

- `GET /healthz`
- `POST /v1/responses`
- `POST /v1/chat/completions`
- `POST /__admin/scenarios`
- `POST /__admin/reset`
- `GET /__admin/requests`

`/v1/*` routes require a non-empty bearer token. Scenarios, request logs, and deterministic response IDs are scoped by bearer token so concurrent test clients can share one server safely.

`/__admin/*` routes are unauthenticated by default, but an optional bearer token selects the same namespace as `/v1/*`. Admin requests with a malformed or empty `Authorization` header are rejected.

## Run locally

```bash
cargo run
```

The server binds to `127.0.0.1:3000` by default.

## Admin scripting

Load deterministic one-shot scenarios:

```bash
curl -X POST http://127.0.0.1:3000/__admin/scenarios \
  -H 'Authorization: Bearer suite-a' \
  -H 'content-type: application/json' \
  -d '{
    "scenarios": [
      {
        "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
        "script": { "kind": "success", "response_text": "scripted reply" }
      }
    ]
  }'
```

Inspect normalized request logs:

```bash
curl http://127.0.0.1:3000/__admin/requests \
  -H 'Authorization: Bearer suite-a'
```

Reset scenarios, logs, and deterministic counters:

```bash
curl -X POST http://127.0.0.1:3000/__admin/reset \
  -H 'Authorization: Bearer suite-a'
```

## Behavior summary

- Non-stream and stream success paths are driven from the same canonical response plan.
- `/v1/responses` and `/v1/chat/completions` share the same deterministic fallback behavior.
- Structured output supports `json_object` and a documented `json_schema` subset.
- Scripted failures support OpenAI-shaped application errors, delays, hangs, partial streams, and malformed SSE.

## Optional Live OpenAI Smoke Suite

Run the ignored live drift detector only when you explicitly want to compare `twin-openai` against the real OpenAI API:

```bash
OPENAI_API_KEY=... cargo test --test live_openai_contract -- --ignored --nocapture
```

Optional environment variables:

- `TWIN_OPENAI_LIVE_MODEL` defaults to `gpt-5-nano-2025-08-07`
- `TWIN_OPENAI_LIVE_BASE_URL` defaults to `https://api.openai.com`
- `OPENAI_ORGANIZATION` and `OPENAI_PROJECT` are forwarded when present

This suite is not part of normal CI. It is intentionally a drift detector for request/response shape and SSE sequencing, so opt-in failures can represent real compatibility gaps rather than a broken local test harness.

If the supplied OpenAI credentials lack required endpoint scopes or quota, the ignored test will skip the blocked live surface instead of reporting protocol drift.

Current live coverage includes `responses` and `chat.completions` text, streaming, structured output, function tools, `tool_choice: "none"` behavior, image-input acceptance, and both non-stream and streamed `responses` continuation turns.

See [docs/compatibility-matrix.md](/Users/bhelmkamp/p/brynary/twin-openai/docs/compatibility-matrix.md) for the supported field matrix and explicit exclusions.
