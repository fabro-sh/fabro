# Compatibility Matrix

Supported endpoints:

- `GET /healthz`
- `POST /v1/responses`
- `POST /v1/chat/completions`
- `POST /__admin/scenarios`
- `POST /__admin/reset`
- `GET /__admin/requests`

State isolation:

- `/v1/*` request state is scoped by bearer token
- admin routes may include the same bearer token to target that namespace
- admin routes without auth operate on the global namespace

Supported `/v1/responses` fields:

- bearer auth
- `stream`
- `metadata`
- `stop`
- `previous_response_id`
- `reasoning`
- `text.format.type = text | json_object | json_schema`
- image inputs in `input[*].content[*].type = input_image`
- scripted tool calls and continuation input items

Supported `/v1/chat/completions` fields:

- bearer auth
- `stream`
- `tools`
- `tool_choice`
- `response_format.type = text | json_object | json_schema`
- `stop`
- reasoning-bearing assistant content

Structured output subset:

- object roots
- primitive property types: `string`, `integer`, `number`, `boolean`
- nested object properties

Unsupported schema constructs fail explicitly:

- arrays
- `anyOf`
- `oneOf`

Failure scripting:

- application errors with explicit status and OpenAI-shaped body
- optional `Retry-After`
- delay before headers
- hang before first byte
- inter-event stream delay
- close stream after N chunks
- malformed/truncated SSE ending
