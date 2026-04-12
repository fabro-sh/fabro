# Server-Canonical Secrets, Doctor, and Repo Init

## Summary

Migrate five CLI command families from local-only to server-canonical:

- **secrets** — move from `~/.fabro/.env` to server-owned JSON store with write-only API
- **provider login** — keep validation in CLI, save credentials via server API
- **repo init** — call server to verify repo access after scaffolding
- **doctor** — replace local probing with a single server diagnostics endpoint
- **health** — add server version to `GET /health` for CLI/server parity checks

After this pass, the CLI has no direct file I/O for secrets and no direct probing of external services for health checks. The server is the single owner of credentials and the single source of diagnostic truth.

This plan deliberately excludes the run manifest (server-canonical `POST /runs` body). That is a separate, larger effort.

## Scope Boundaries

In scope:
- add `version` to `GET /health`
- server-side secret store (JSON file, in-memory cache, store-backed secret accessors)
- `PUT /api/v1/secrets/{name}`, `DELETE /api/v1/secrets/{name}`, `GET /api/v1/secrets`
- rewrite `fabro secret set`, `fabro secret list`, `fabro secret rm` as API clients
- remove `fabro secret get` (secrets are write-only)
- rewrite `fabro provider login` to save credentials via the server
- update `fabro install` to save GitHub App secrets via the local server and print restart guidance when needed
- `GET /api/v1/repos/github/{owner}/{name}` for repo access checks
- update `fabro repo init` to call repo endpoint
- `POST /api/v1/health/diagnostics` with server-side health probing
- rewrite `fabro doctor` as API client + version parity check + retained local config warning
- demo mode handlers for all new endpoints
- add shared `--server <target>` support for these server-canonical CLI commands, where `<target>` is either an HTTP(S) base URL or an absolute Unix socket path

Out of scope:
- run manifest / workflow packaging
- changes to `fabro exec` credential handling (`OPENAI_API_KEY=secret fabro exec ...` is the intended path)
- remote server auth (mTLS, JWT) — endpoints follow existing auth patterns
- encrypted-at-rest secret storage — JSON file with filesystem permissions is sufficient for now
- openssl system dependency check (being removed soon)
- broader CLI target cleanup outside the command families touched by this plan

## Key Decisions

- Secrets are **write-only**. No endpoint exposes secret values after they are stored. `GET /api/v1/secrets` returns names and timestamps only. `fabro secret get` is removed.
- Secret storage is a JSON file at `<data_dir>/secrets.json` under the active server data dir.
- The server does **not** mutate process env vars on secret writes. Server-side secret consumers read through a shared store-backed adapter so updated secrets take effect immediately for request-time flows.
- Startup-time components that only initialize once at server boot are allowed to require restart after credential changes. `fabro install` should print that restart requirement when it detects the server was already running.
- `GET /health` gains a `version` field. The CLI checks version parity before rendering diagnostics.
- `POST /api/v1/health/diagnostics` (not GET) because it triggers expensive external probes (LLM providers, GitHub, sandbox). The response reuses the shape of the existing `CheckReport` struct.
- `GET /api/v1/repos/github/{owner}/{name}` is intentionally GitHub-specific in the URL. Another segment can be added for other providers later.
- `provider login` keeps its interactive prompting and OAuth flow on the CLI side. After obtaining credentials, it saves them via `PUT /api/v1/secrets/{name}`.
- `doctor` keeps one local CLI check for user config files and legacy `.env` warning, then requires a connected server for everything else. There is no fast/offline mode.
- `dot` system dependency check moves server-side. `openssl` check is dropped. `node` check is dropped (build-time dependency only).
- The `--show-values` flag on `fabro secret list` is removed as a consequence of the write-only secret model.
- `PUT /api/v1/secrets/{name}` accepts any env-var-like key name and rejects invalid names with `400`.
- These server-canonical CLI commands use one explicit override flag, `--server <target>`, where `<target>` is either an HTTP(S) URL or an absolute Unix socket path. When omitted, they connect to the local server for the active storage dir, starting it if necessary.
- `[server].base_url` is replaced by `[server].target`, using the same string syntax as `--server`.
- `fabro install` is local-only. It never targets a remote server.
- All new endpoints have demo mode handlers.

## Implementation Changes

### 1. Add version to `GET /health`

In `docs/api-reference/fabro-api.yaml`:
- add `version` field to `HealthResponse` schema:
  ```yaml
  HealthResponse:
    description: Service health check response.
    type: object
    required:
      - status
      - version
    properties:
      status:
        type: string
        description: Health status indicator.
        example: ok
      version:
        type: string
        description: Server version string.
        example: "0.176.2"
  ```

In `lib/crates/fabro-server/src/server.rs`:
- update the `health` handler to include the version:
  ```rust
  async fn health() -> Response {
      Json(serde_json::json!({
          "status": "ok",
          "version": fabro_util::version::FABRO_VERSION,
      }))
      .into_response()
  }
  ```

Rebuild `fabro-api` to pick up the schema change:
```
cargo build -p fabro-api
```

Add a test in `server.rs` inline tests:
- send `GET /health`, assert status 200, assert `version` field is a non-empty string, assert `status` is `"ok"`.

### 2. Server-side secret store

Create `lib/crates/fabro-server/src/secret_store.rs`.

This module owns a JSON file and an in-memory cache:

```rust
pub struct SecretEntry {
    pub value: String,
    pub created_at: String,  // ISO 8601
    pub updated_at: String,  // ISO 8601
}

pub struct SecretMetadata {
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

pub struct SecretStore {
    path: PathBuf,
    entries: HashMap<String, SecretEntry>,
}
```

Public API:
- `SecretStore::load(path: PathBuf) -> Result<Self>` — reads JSON file (or creates empty if missing), parses into `entries`.
- `store.set(name: &str, value: &str) -> Result<SecretMetadata>` — validates the key name, upserts entry with current timestamp, writes atomically (write to temp file, rename to `secrets.json`), returns metadata.
- `store.remove(name: &str) -> Result<()>` — validates the key name, removes entry (error if not found), writes atomically (write to temp file, rename).
- `store.list() -> Vec<SecretMetadata>` — returns names + timestamps, sorted by name. No values.
- `store.get(name: &str) -> Option<&str>` — reads a single secret value for server-side consumers.
- `store.snapshot() -> HashMap<String, String>` — clones the current key/value map for request-time consumers that need a full view.
- `SecretStore::validate_name(name: &str) -> Result<()>` — accept only env-var-like keys (`[A-Za-z_][A-Za-z0-9_]*`).

The JSON file format:
```json
{
  "ANTHROPIC_API_KEY": {
    "value": "sk-ant-...",
    "created_at": "2026-04-05T10:30:00Z",
    "updated_at": "2026-04-05T10:30:00Z"
  }
}
```

File permissions: `0o600` on Unix (same as current `.env`).

Inline tests in `secret_store.rs`:
- `load` from empty/missing file returns empty store
- `set` creates entry, verify file written
- `set` existing key updates `updated_at`, preserves `created_at`
- `remove` deletes entry, verify file written
- `remove` missing key returns error
- `list` returns sorted metadata without values
- invalid names are rejected
- use `tempdir` for file paths in tests

In `lib/crates/fabro-server/src/server.rs`:
- add `pub secret_store: tokio::sync::RwLock<SecretStore>` to `AppState`
- update `build_app_state` to derive `secrets.json` from the active server data dir and call `SecretStore::load(path)?`
- update `create_app_state` (test helper) to use a temp path

In `lib/crates/fabro-server/src/lib.rs`:
- add `pub mod secret_store;`

Also in the server layer:
- add small store-backed adapters for the server-side secret consumers touched by this plan instead of continuing to call `std::env::var(...)` / `from_env()`
- the adapters only need to cover the flows touched by this plan:
  - LLM client construction for diagnostics/model probing
  - GitHub App credentials for repo checks
  - GitHub client secret and session secret reads in web auth
  - diagnostics secret presence/probe checks
- request-time server flows should read from the current `SecretStore` snapshot so new credentials take effect immediately
- startup-time flows may continue to require restart if they only read credentials during boot

### 3. Secret CRUD API endpoints

In `docs/api-reference/fabro-api.yaml`:
- add schemas:
  ```yaml
  SetSecretRequest:
    description: Request to store a secret value.
    type: object
    required:
      - value
    properties:
      value:
        type: string
        description: The secret value to store.

  SecretMetadata:
    description: Metadata for a stored secret (value is never exposed).
    type: object
    required:
      - name
      - created_at
      - updated_at
    properties:
      name:
        type: string
        description: Secret key name.
        example: ANTHROPIC_API_KEY
      created_at:
        type: string
        format: date-time
        description: When the secret was first stored.
      updated_at:
        type: string
        format: date-time
        description: When the secret was last updated.

  SecretListResponse:
    description: List of stored secret metadata.
    type: object
    required:
      - data
    properties:
      data:
        type: array
        items:
          $ref: "#/components/schemas/SecretMetadata"
  ```

- add paths:
  ```yaml
  /api/v1/secrets:
    get:
      operationId: listSecrets
      tags: [Secrets]
      summary: List stored secrets (names and timestamps only).
      responses:
        "200":
          description: Secret metadata list.
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/SecretListResponse"

  /api/v1/secrets/{name}:
    put:
      operationId: setSecret
      tags: [Secrets]
      summary: Store or update a secret.
      parameters:
        - name: name
          in: path
          required: true
          schema:
            type: string
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/SetSecretRequest"
      responses:
        "200":
          description: Secret stored.
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/SecretMetadata"
        "400":
          description: Invalid secret name or request body.
    delete:
      operationId: deleteSecret
      tags: [Secrets]
      summary: Delete a stored secret.
      parameters:
        - name: name
          in: path
          required: true
          schema:
            type: string
      responses:
        "204":
          description: Secret deleted.
        "400":
          description: Invalid secret name.
        "404":
          description: Secret not found.
        "500":
          description: Secret store write failed.
  ```

Rebuild `fabro-api`:
```
cargo build -p fabro-api
```

In `lib/crates/fabro-server/src/server.rs`:
- add handlers:

  ```rust
  async fn list_secrets(
      _auth: AuthenticatedService,
      State(state): State<Arc<AppState>>,
  ) -> Response {
      let store = state.secret_store.read().await;
      let data = store.list();
      (StatusCode::OK, Json(serde_json::json!({ "data": data }))).into_response()
  }

  async fn set_secret(
      _auth: AuthenticatedService,
      State(state): State<Arc<AppState>>,
      Path(name): Path<String>,
      Json(body): Json<types::SetSecretRequest>,
  ) -> Response {
      let mut store = state.secret_store.write().await;
      match store.set(&name, &body.value) {
          Ok(meta) => (StatusCode::OK, Json(meta)).into_response(),
          Err(SecretStoreError::InvalidName(_)) => {
              ApiError::new(StatusCode::BAD_REQUEST, "invalid secret name").into_response()
          }
          Err(e) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
      }
  }

  async fn delete_secret(
      _auth: AuthenticatedService,
      State(state): State<Arc<AppState>>,
      Path(name): Path<String>,
  ) -> Response {
      let mut store = state.secret_store.write().await;
      match store.remove(&name) {
          Ok(()) => StatusCode::NO_CONTENT.into_response(),
          Err(SecretStoreError::InvalidName(_)) => StatusCode::BAD_REQUEST.into_response(),
          Err(SecretStoreError::NotFound(_)) => StatusCode::NOT_FOUND.into_response(),
          Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
      }
  }
  ```

- update the `axum::routing` import to include `put` and `delete`
- ensure request-body logging/redaction treats `PUT /secrets/{name}` values as sensitive and never logs the raw secret
- add routes in `real_routes()`:
  ```rust
  .route("/secrets", get(list_secrets))
  .route("/secrets/{name}", put(set_secret).delete(delete_secret))
  ```

- add routes in `demo_routes()` pointing to demo handlers (see §10).

Tests in `server.rs` inline tests:
- `PUT /secrets/TEST_KEY` with `{"value": "test-val"}` → 200, response has `name`, `created_at`, `updated_at`
- `GET /secrets` → 200, `data` array contains the key just set, no `value` field present
- `PUT` same key again → 200, `updated_at` changes
- `PUT /secrets/NOT-VALID` → 400
- `DELETE /secrets/TEST_KEY` → 204
- `DELETE /secrets/NONEXISTENT` → 404
- `GET /secrets` after delete → empty `data`

### 4. Rewrite `fabro secret` CLI commands as API clients

In `lib/crates/fabro-cli/src/commands/secret/mod.rs`:
- remove `SecretCommand::Get` variant
- make `execute` async (it currently dispatches to sync functions)
- each subcommand uses the shared explicit server-target helper, then the generated `fabro_api::Client`

In `lib/crates/fabro-cli/src/args.rs`:
- add `ServerTargetArgs`:
  ```rust
  #[derive(Args, Debug, Clone, Default)]
  pub(crate) struct ServerTargetArgs {
      /// Fabro server target: http(s) URL or absolute Unix socket path
      #[arg(long = "server", env = "FABRO_SERVER")]
      pub(crate) target: Option<String>,
  }
  ```
- flatten `ServerTargetArgs` into:
  - `SecretNamespace`
  - `ProviderLoginArgs`
  - `DoctorArgs` (convert the inline `Doctor { ... }` variant to a named args struct)
  - `RepoInitArgs` (convert the inline `RepoCommand::Init { ... }` variant to a named args struct)
- do **not** add `ServerTargetArgs` to `InstallArgs`; `install` stays local-only

In `lib/crates/fabro-cli/src/user_config.rs`:
- replace `[server].base_url` support with `[server].target`
- add a shared parser/resolver for explicit server targets used by these command families:
  - `http://...` or `https://...` => remote HTTP target
  - absolute path => Unix socket target
  - anything else => clear parse error
- when `ServerTargetArgs` is absent, fall back to the local storage-dir/default-storage-dir server and auto-start it if necessary
- use this helper from `secret`, `provider login`, `repo init`, and `doctor`

In `lib/crates/fabro-cli/src/commands/secret/set.rs`:
- replace the body with:
  ```rust
  pub(super) async fn set_command(
      args: &SecretSetArgs,
      server: &ServerTargetArgs,
      globals: &GlobalArgs,
  ) -> Result<()> {
      let client = secret_client(server).await?;
      let meta = client.set_secret()
          .name(&args.key)
          .body(types::SetSecretRequest { value: args.value.clone() })
          .send()
          .await?;
      if globals.json {
          print_json_pretty(&meta)?;
      } else {
          eprintln!("Set {}", args.key);
      }
      Ok(())
  }
  ```

In `lib/crates/fabro-cli/src/commands/secret/list.rs`:
- remove `--show-values` flag from `SecretListArgs`
- replace the body with:
  ```rust
  pub(super) async fn list_command(
      args: &SecretListArgs,
      server: &ServerTargetArgs,
      globals: &GlobalArgs,
  ) -> Result<()> {
      let client = secret_client(server).await?;
      let resp = client.list_secrets().send().await?;
      if globals.json {
          print_json_pretty(&resp.data)?;
      } else {
          for secret in &resp.data {
              println!("{}\t{}", secret.name, secret.updated_at);
          }
      }
      Ok(())
  }
  ```

In `lib/crates/fabro-cli/src/commands/secret/rm.rs`:
- replace the body with:
  ```rust
  pub(super) async fn rm_command(
      args: &SecretRmArgs,
      server: &ServerTargetArgs,
      globals: &GlobalArgs,
  ) -> Result<()> {
      let client = secret_client(server).await?;
      client.delete_secret().name(&args.key).send().await?;
      if globals.json {
          print_json_pretty(&serde_json::json!({ "key": args.key }))?;
      } else {
          eprintln!("Removed {}", args.key);
      }
      Ok(())
  }
  ```

Delete `lib/crates/fabro-cli/src/commands/secret/get.rs`.

In `lib/crates/fabro-cli/src/args.rs`:
- remove `SecretCommand::Get` and `SecretGetArgs`

Update any integration tests that test `fabro secret get` — remove them.

### 5. Rewrite `fabro provider login` to save via server

In `lib/crates/fabro-cli/src/commands/provider/login.rs`:
- after obtaining validated `env_pairs` (the `Vec<(String, String)>` of env var name → key value), replace the `provider_auth::write_env_file(...)` call with API calls:
  ```rust
  let client = provider_secret_client(&args.server).await?;
  for (env_var, key) in &env_pairs {
      client.set_secret()
          .name(env_var)
          .body(types::SetSecretRequest { value: key.clone() })
          .send()
          .await
          .with_context(|| format!("failed to save {env_var} to server"))?;
  }
  ```
- remove the `provider_auth::write_env_file` call
- add a temporary warning if a legacy `.env` file exists under the active local storage dir:
  ```
  Warning: ~/.fabro/.env is no longer read by fabro server. Re-enter credentials with `fabro provider login` or `fabro secret set`.
  ```

In `lib/crates/fabro-cli/src/shared/provider_auth.rs`:
- `write_env_file` may become dead code after this change. If no other callers exist, delete it.
- `validate_api_key` currently calls `std::env::set_var` temporarily to validate. This still works because validation happens before saving. However, consider whether the validation should instead construct the LLM client explicitly with the key rather than mutating process env. This is a follow-up concern — for now the existing validation approach is fine since the CLI process is single-threaded for this flow.

### 5a. Update `fabro install` to save GitHub App secrets via the local server

In `lib/crates/fabro-cli/src/commands/install.rs`:
- keep `install` local-only. It should always operate on the local server for the active storage dir and never accept `--server`
- after writing `server.toml` / `user.toml` and producing GitHub App secret env pairs, persist those secret values via the local server's `PUT /secrets/{name}` API instead of writing `.env`
- detect whether the local server was already running before `install`
- if the server was not running, letting the local client auto-start it is fine
- if the server was already running, print a clear restart warning after saving secrets:
  ```
  Fabro server was already running. Restart it to pick up startup-time credential changes (for example webhook listener configuration).
  ```
- remove the `.env` reload
- update the final doctor invocation to the new signature / args shape

This is intentionally a hard break from `.env`, but `install` should print a temporary migration warning if it sees a legacy `.env` file in the local storage dir.

### 6. `GET /api/v1/repos/github/{owner}/{name}` endpoint

In `docs/api-reference/fabro-api.yaml`:
- add schema:
  ```yaml
  RepoCheckResponse:
    description: Repository access check result.
    type: object
    required:
      - owner
      - name
      - accessible
    properties:
      owner:
        type: string
        description: GitHub repository owner.
        example: acme-corp
      name:
        type: string
        description: GitHub repository name.
        example: my-app
      accessible:
        type: boolean
        description: Whether the server has read-write access to this repository.
      default_branch:
        type: string
        nullable: true
        description: Default branch name, if accessible.
        example: main
      private:
        type: boolean
        nullable: true
        description: Whether the repository is private, if accessible.
      permissions:
        type: object
        nullable: true
        description: Detected permission levels.
        properties:
          pull:
            type: boolean
          push:
            type: boolean
          admin:
            type: boolean
      install_url:
        type: string
        nullable: true
        description: GitHub App installation URL when the repo is not yet accessible.
  ```

- add path:
  ```yaml
  /api/v1/repos/github/{owner}/{name}:
    get:
      operationId: getGithubRepo
      tags: [Repos]
      summary: Check server access to a GitHub repository.
      parameters:
        - name: owner
          in: path
          required: true
          schema:
            type: string
        - name: name
          in: path
          required: true
          schema:
            type: string
      responses:
        "200":
          description: Repository access details.
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/RepoCheckResponse"
  ```

Rebuild `fabro-api`:
```
cargo build -p fabro-api
```

In `lib/crates/fabro-server/Cargo.toml`:
- add `fabro-github` as a dependency if not already present

In `lib/crates/fabro-server/src/server.rs`:
- add handler:
  ```rust
  async fn get_github_repo(
      _auth: AuthenticatedService,
      State(state): State<Arc<AppState>>,
      Path((owner, name)): Path<(String, String)>,
  ) -> Response
  ```
	  The handler:
	  1. Reads non-secret GitHub App config (`app_id`, `slug`) from `Settings`. Reads `GITHUB_APP_PRIVATE_KEY` from `SecretStore`.
	  2. Signs a JWT via `fabro_github::sign_app_jwt`.
	  3. Calls `GET /repos/{owner}/{name}/installation` to check if the App is installed.
	  4. If installed, mints an installation token and calls `GET /repos/{owner}/{name}` to get repo details (default branch, private flag, permissions).
	  5. If not installed, returns `accessible: false` with null optional fields and, when possible, an `install_url`.
	  6. Returns `RepoCheckResponse`.

  If GitHub App credentials are not configured (missing from settings or secret store), return `accessible: false` with a descriptive error or a 503.

- add route in `real_routes()`:
  ```rust
  .route("/repos/github/{owner}/{name}", get(get_github_repo))
  ```
- add route in `demo_routes()` pointing to demo handler (see §10).

Tests:
- Testing the real handler requires mocking the GitHub API or the `HttpClient` trait. Use a unit test that exercises the response shape with a mock `AppState` that has a test GitHub client, or test at the integration level with the demo handler.
- At minimum, test the demo handler returns 200 with the expected shape.

### 7. Update `fabro repo init` to call repo endpoint

In `lib/crates/fabro-cli/src/commands/repo/init.rs`:
- replace `check_github_app_installation()` with a server call:
  ```rust
  async fn check_repo_access(owner: &str, name: &str, args: &RepoInitArgs) -> Result<()> {
      let client = repo_client(&args.server).await?;
      let resp = client.get_github_repo()
          .owner(owner)
          .name(name)
          .send()
          .await?;
      if resp.accessible {
          println!("  {} GitHub repo {}/{} is accessible", green_check, owner, name);
          if let Some(branch) = &resp.default_branch {
              println!("     Default branch: {branch}");
          }
      } else {
          println!("  {} GitHub repo {}/{} is not accessible", yellow_warn, owner, name);
          println!("     Install the GitHub App to enable PR creation and webhook triggers.");
          if let Some(url) = &resp.install_url {
              println!("     Install at: {url}");
          }
      }
      Ok(())
  }
  ```
- The function still parses the git remote to extract `owner`/`name` — that stays CLI-side since it reads the local git config.
- Remove the direct `fabro_github::sign_app_jwt`, `fabro_github::check_app_installed`, `build_github_app_credentials` calls.
- Keep the `fabro-github` dependency in `fabro-cli/Cargo.toml` — it has many other callers (pr/*, preflight.rs, shared/github.rs).
- preserve the current interactive UX:
  - when the repo is not yet accessible and stdin is a terminal, print the install URL, wait for Enter, then call the repo endpoint again
  - print the second check result after the re-check

### 8. `POST /api/v1/health/diagnostics` endpoint

In `docs/api-reference/fabro-api.yaml`:
- add schemas:
  ```yaml
  DiagnosticsReport:
    description: Server health diagnostics report.
    type: object
    required:
      - version
      - sections
    properties:
      version:
        type: string
        description: Server version.
      sections:
        type: array
        items:
          $ref: "#/components/schemas/DiagnosticsSection"

  DiagnosticsSection:
    type: object
    required:
      - title
      - checks
    properties:
      title:
        type: string
      checks:
        type: array
        items:
          $ref: "#/components/schemas/DiagnosticsCheck"

  DiagnosticsCheck:
    type: object
    required:
      - name
      - status
      - summary
    properties:
      name:
        type: string
      status:
        type: string
        enum:
          - pass
          - warning
          - error
      summary:
        type: string
      details:
        type: array
        items:
          $ref: "#/components/schemas/DiagnosticsDetail"
      remediation:
        type: string
        nullable: true

  DiagnosticsDetail:
    type: object
    required:
      - text
      - warn
    properties:
      text:
        type: string
      warn:
        type: boolean
  ```

- add path:
  ```yaml
  /api/v1/health/diagnostics:
    post:
      operationId: runDiagnostics
      tags: [Discovery]
      summary: Run server health diagnostics.
      description: Probes external services (LLM providers, GitHub, sandbox) and checks server configuration. May be slow.
      responses:
        "200":
          description: Diagnostics report.
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/DiagnosticsReport"
  ```

Rebuild `fabro-api`:
```
cargo build -p fabro-api
```

Create `lib/crates/fabro-server/src/diagnostics.rs`.

This module contains the server-side check functions. Many can be adapted from the existing `doctor.rs` in `fabro-cli`. The key checks, grouped into sections:

**Section "Credentials":**
- `check_llm_providers` — for each provider in `Provider::ALL`, check if a secret exists in the store and probe connectivity by sending a test message.
- `check_github_app` — check that `GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY`, etc. exist in settings/store, validate JWT signing, and probe `GET /app` on GitHub API.
- `check_sandbox` — check `DAYTONA_API_KEY` exists. Probe Daytona API.
- `check_brave_search` — check `BRAVE_SEARCH_API_KEY` exists. Probe Brave API.

**Section "System":**
- `check_system_dep_dot` — check `dot` is in PATH and version ≥ 2.0.0.

**Section "Configuration":**
- `check_crypto` — validate mTLS certs/keys, JWT keys, session secret (same checks as current doctor).

Dropped from diagnostics (compared to current doctor):
- `check_api` / `check_web` — the CLI's ability to call the diagnostics endpoint is itself the connectivity check. No circular self-check.
- `check_system_dep_node` — node is a build-time dependency only, not needed at server runtime.
- `check_system_dep_openssl` — being removed soon.

The handler:

```rust
async fn run_diagnostics(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
) -> Response {
    let report = diagnostics::run_all(&state).await;
    (StatusCode::OK, Json(report)).into_response()
}
```

`diagnostics::run_all` runs all checks concurrently (where possible) and returns a `DiagnosticsReport`. The probes (LLM, GitHub, Daytona, Brave) should be run concurrently via `tokio::join!` or `futures::join!`.
- apply explicit timeouts to the live probes so `doctor` cannot hang indefinitely
- keep concurrency bounded to this fixed set of checks; do not allow unbounded fan-out

Each check function returns a `DiagnosticsCheck` struct that maps 1:1 to the API schema. The existing `CheckResult` from `fabro_util::check_report` is very close — consider either:
- reusing `CheckResult` directly and serializing it (it already has `Serialize`)
- or mapping to the generated `fabro_api` types

For the wire contract, prefer an explicit conversion step rather than assuming `CheckResult` serialization is automatically stable enough for the API surface.

Add route in `real_routes()`:
```rust
.route("/health/diagnostics", post(run_diagnostics))
```

Add route in `demo_routes()` pointing to demo handler (see §10).

Tests:
- test that `POST /health/diagnostics` returns 200 with a `version` field and a non-empty `sections` array
- test that each section has a `title` and `checks` array
- test the demo handler returns the same shape

### 9. Rewrite `fabro doctor` as API client

In `lib/crates/fabro-cli/src/commands/doctor.rs`:
- replace `run_doctor` with a thin client:
  ```rust
  pub async fn run_doctor(args: &DoctorArgs, globals: &GlobalArgs) -> Result<()> {
      let client = doctor_client(&args.server).await?;

      // Local config warning block
      let local_checks = render_local_config_checks()?;

      // Version parity check
      let health = client.get_health().send().await?;
      let server_version = &health.version;
      let cli_version = fabro_util::version::FABRO_VERSION;

      // Run diagnostics
      let report = client.run_diagnostics().send().await?;

      if globals.json {
          print_json_pretty(&report)?;
          return Ok(());
      }

      // Version parity
      if server_version != cli_version {
          eprintln!(
              "⚠ Version mismatch: CLI={cli_version} Server={server_version}"
          );
      }

      // Render the retained local config warnings first, then the server diagnostics report.
      render_local_checks(&local_checks);
      render_diagnostics(&report);

      Ok(())
  }
  ```
- the rendering logic from `CheckReport::render()` can be reused. Either:
  - convert the API response into a `CheckReport` and call `render()`
  - or extract the rendering logic into a function that takes the diagnostics fields directly
- remove the local dry-run / offline mode flag
- keep the local user-config and legacy `.env` checks
- remove the other local check functions (`check_llm_providers`, `check_github_app`, `check_sandbox`, `check_brave_search`, `check_system_deps`, `check_api`, `check_web`, `check_crypto`)
- remove the corresponding test functions if they only tested the removed local checks
- keep the `CheckReport`/`CheckResult` rendering code in `fabro_util::check_report` — it is still useful for rendering the server's response

If the server client connection fails (server unreachable), the error message should be clear:
```
Error: could not connect to fabro server. Run `fabro server start` or check `fabro server status`.
```

Also:
- if a legacy local `.env` file exists, print a temporary warning that the server no longer reads it
- `doctor` should still succeed in rendering that local warning even when the diagnostics call later fails

### 10. Demo mode handlers

In `lib/crates/fabro-server/src/demo/mod.rs`:

**Secrets demo:**
- maintain a static in-memory `HashMap` with pre-populated fake secrets:
  ```rust
  pub(crate) async fn list_secrets(...) -> Response {
      let data = vec![
          serde_json::json!({
              "name": "ANTHROPIC_API_KEY",
              "created_at": "2026-01-15T09:00:00Z",
              "updated_at": "2026-03-20T14:30:00Z",
          }),
          serde_json::json!({
              "name": "OPENAI_API_KEY",
              "created_at": "2026-01-15T09:05:00Z",
              "updated_at": "2026-02-10T11:00:00Z",
          }),
          serde_json::json!({
              "name": "GITHUB_APP_PRIVATE_KEY",
              "created_at": "2026-01-15T09:10:00Z",
              "updated_at": "2026-01-15T09:10:00Z",
          }),
      ];
      (StatusCode::OK, Json(serde_json::json!({ "data": data }))).into_response()
  }

  pub(crate) async fn set_secret(...) -> Response {
      // return fake metadata with current timestamps
  }

  pub(crate) async fn delete_secret(...) -> Response {
      StatusCode::NO_CONTENT.into_response()
  }
  ```

**Repo demo:**
- return a fake accessible repo:
  ```rust
  pub(crate) async fn get_github_repo(
      ...,
      Path((owner, name)): Path<(String, String)>,
  ) -> Response {
      (StatusCode::OK, Json(serde_json::json!({
          "owner": owner,
          "name": name,
          "accessible": true,
          "default_branch": "main",
          "private": false,
          "permissions": { "pull": true, "push": true, "admin": false },
      }))).into_response()
  }
  ```

**Diagnostics demo:**
- return an all-passing report:
  ```rust
  pub(crate) async fn run_diagnostics(...) -> Response {
      (StatusCode::OK, Json(serde_json::json!({
          "version": fabro_util::version::FABRO_VERSION,
          "sections": [
              {
                  "title": "Credentials",
                  "checks": [
                      { "name": "LLM Providers", "status": "pass", "summary": "Anthropic, OpenAI configured", "details": [], "remediation": null },
                      { "name": "GitHub App", "status": "pass", "summary": "JWT signing OK", "details": [], "remediation": null },
                      { "name": "Sandbox", "status": "pass", "summary": "Daytona reachable", "details": [], "remediation": null },
                      { "name": "Brave Search", "status": "pass", "summary": "API key configured", "details": [], "remediation": null },
                  ]
              },
              {
                  "title": "System",
                  "checks": [
                      { "name": "dot", "status": "pass", "summary": "dot 12.1.2", "details": [], "remediation": null },
                  ]
              },
              {
                  "title": "Configuration",
                  "checks": [
                      { "name": "Crypto", "status": "pass", "summary": "All keys valid", "details": [], "remediation": null },
                  ]
              },
          ]
      }))).into_response()
  }
  ```

Wire all demo handlers in `demo_routes()`:
```rust
.route("/secrets", get(demo::list_secrets))
.route("/secrets/{name}", put(demo::set_secret).delete(demo::delete_secret))
.route("/repos/github/{owner}/{name}", get(demo::get_github_repo))
.route("/health/diagnostics", post(demo::run_diagnostics))
```

## Implementation Order

```
1  Health version                (no deps, small)
2  Secret store + adapters       (no deps)
3  Secret CRUD API               (depends on 2)
4  Shared server target plumbing (parallel with 2-3)
5  Secret CLI migration          (depends on 3, 4)
6  Provider login migration      (depends on 3, 4)
7  Install migration             (depends on 3)
8  Repo check API                (depends on 2)
9  Repo init migration           (depends on 4, 8)
10 Diagnostics API               (depends on 2)
11 Doctor CLI migration          (depends on 1, 4, 10)
```

Steps 1, 2, and 4 can start in parallel. Steps 5 and 6 can run in parallel once 3 and 4 are done.

## Resolved Questions

1. **Secret store path**: `<data_dir>/secrets.json` under the active server data dir. Credentials are owned by the server instance, not by a global shared file.

2. **Migration from existing `.env`**: no auto-import. Hard break. Users must re-enter credentials via `fabro provider login` or `fabro secret set`, and the CLI/server should emit a temporary warning when they detect a legacy `.env`.

3. **GitHub App credentials for repo check**: non-secret config (`app_id`, `slug`) goes in server settings (not mixed with secrets). Secret values (`GITHUB_APP_PRIVATE_KEY`) go in the secret store. The repo check handler reads `app_id`/`slug` from `Settings` and `GITHUB_APP_PRIVATE_KEY` from `SecretStore`.

4. **Diagnostics: `check_api` and `check_web`**: dropped. The CLI's ability to call the diagnostics endpoint *is* the API connectivity check — if the server is unreachable, the CLI gets a connection error before any diagnostics run. No circular self-check needed. The one retained local CLI check is user config / legacy `.env`.

5. **`node` dependency**: dropped from diagnostics. The web app is an SPA served by the Rust server; `node` is a build-time dependency only, not needed at server runtime.

6. **`dot` dependency**: moves server-side. `openssl` dependency: dropped (being removed soon).

7. **Server targeting**: server-canonical admin commands use `--server <target>` and `[server].target`, where `<target>` is either an HTTP(S) base URL or an absolute Unix socket path.

8. **`fabro install` targeting**: `install` is local-only and writes config plus secrets for the local server host. If the server was already running, `install` prints that a restart is required for startup-time features to pick up new secrets.
