All 390 tests pass and clippy is clean with zero warnings.

Here's a summary of the changes made to `lib/crates/fabro-llm/src/providers/anthropic.rs`:

1. **Added `output_config` field to `ApiRequest` struct** (after `thinking`):
   ```rust
   #[serde(skip_serializing_if = "Option::is_none")]
   output_config: Option<serde_json::Value>,
   ```

2. **Mapped `reasoning_effort` → `output_config` in `build_api_request`** — converts `request.reasoning_effort` into `{"effort": "<value>"}` JSON, mirroring how the OpenAI provider maps this field:
   ```rust
   let output_config = request
       .reasoning_effort
       .as_ref()
       .map(|effort| serde_json::json!({"effort": effort}));
   ```

3. **Added two tests**:
   - `build_api_request_maps_reasoning_effort_to_output_config` — verifies `reasoning_effort: Some("medium")` produces `output_config: Some({"effort": "medium"})`
   - `build_api_request_omits_output_config_when_no_reasoning_effort` — verifies `reasoning_effort: None` produces `output_config: None`

4. **Updated all existing `ApiRequest` construction sites** in tests to include `output_config: None`.