# Run Manifest and Preflight

## Summary

Replace the current `POST /runs` request — which sends a filesystem path and relies on the server reading workflow definition files from disk — with a self-contained **run manifest**. The CLI gathers all workflow-definition inputs (DOT source, TOML configs, referenced prompt files, imported graphs, child workflows) into a single JSON payload. The server owns all interpretation: config merging, variable expansion, transforms, validation.

This also introduces `POST /api/v1/preflight`, which accepts the same manifest and returns a structured health report without creating a run.

After this change, the server no longer reads workflow/config/prompt/import files from the CLI's filesystem. It still uses the manifest `cwd` / resolved working directory for execution context (for example local sandbox and repo-aware behavior). The path-based `workflow_path` submission mode is removed.

## Scope Boundaries

In scope:
- define the `RunManifest` schema in the OpenAPI spec
- CLI-side manifest builder that walks the workflow tree and bundles all referenced files
- file resolver abstraction for transforms (bundle-backed instead of disk-backed)
- refactor `FileInliningTransform` and `ImportTransform` to use file resolver
- server-side config resolution from manifest layers (args, workflow TOML, project TOML, user TOML)
- child workflow resolution from the manifest's workflow map
- replace `POST /api/v1/runs` request body with the manifest
- new `POST /api/v1/preflight` endpoint using the same manifest
- update `fabro run`, `fabro create`, `fabro preflight` to build and send manifests
- demo mode for preflight and updated run creation

Out of scope:
- changes to run execution, checkpointing, or resume
- changes to sandbox creation or the execution engine
- changes to `fabro exec`
- encrypted or compressed manifests
- manifest size limits or streaming upload

## Manifest Shape

```json
{
  "version": 1,
  "run_id": "01HV6D7S5YF4Z4B2M7K4N0Q6T9",
  "cwd": "/Users/user/p/my-project",
  "git": {
    "origin_url": "https://github.com/acme/my-app.git",
    "branch": "feature/foo",
    "sha": "abc123",
    "clean": true
  },
  "goal": {
    "type": "file",
    "path": "goal.md",
    "text": "Build and test the app..."
  },
  "args": {
    "model": "claude-opus-4-6",
    "sandbox": "local"
  },
  "target": {
    "identifier": "smoke",
    "path": "fabro/workflows/smoke/workflow.fabro"
  },
  "configs": [
    { "type": "project", "path": "fabro.toml", "source": "[fabro]\nroot = \"fabro/\"\n..." },
    { "type": "user", "path": "/Users/user/.fabro/user.toml", "source": "..." }
  ],
  "workflows": {
    "fabro/workflows/smoke/workflow.fabro": {
      "source": "digraph { ... }",
      "config": {
        "path": "fabro/workflows/smoke/workflow.toml",
        "source": "version = 1\n[vars]\nlanguage = \"rust\""
      },
      "files": {
        "prompts/review.md": {
          "content": "You are a code reviewer...",
          "ref": { "type": "file_inline", "original": "@prompts/review.md", "from": "workflow.fabro" }
        },
        "validate.fabro": {
          "content": "digraph { ... }",
          "ref": { "type": "import", "original": "./validate.fabro", "from": "workflow.fabro" }
        }
      }
    },
    "fabro/workflows/implement-plan/workflow.fabro": {
      "source": "digraph { ... }",
      "files": {
        "prompts/simplify.md": {
          "content": "...",
          "ref": { "type": "file_inline", "original": "@prompts/simplify.md", "from": "workflow.fabro" }
        }
      }
    }
  }
}
```

Field semantics:
- `version` — manifest schema version, currently `1`
- `run_id` — optional pre-generated run ID. Used by detached/local create flows that allocate the run ID in the CLI before submission
- `cwd` — the CLI's working directory at invocation time
- `git` — optional, observable git state from the CLI's working directory. Omitted if not in a git repo
  - `origin_url` — remote origin URL, **sanitized** (credentials stripped from HTTPS URLs to prevent token leakage)
  - `branch` — current branch name
  - `sha` — current commit SHA
  - `clean` — whether the working tree has uncommitted changes
- `goal` — resolved goal with provenance, always includes `text` (the content) and `type` (`"value"` for literal string, `"file"` for file-sourced, `"graph"` for graph-attribute-sourced). When `type` is `"file"`, includes `path` (original file path from TOML or CLI `--goal-file`). The server uses `text` directly and clears any merged `goal_file` path
- `args` — command-local run/preflight args that affect run settings. Sparse: omitted flags are absent. This is not a generic env layer or a dump of global CLI flags
- `target.identifier` — what the user typed (slug like `"smoke"` or path like `"./custom.fabro"`)
- `target.path` — resolved path, keys into the `workflows` map
- `configs` — non-workflow config sources, each with `type` (`"project"` or `"user"`), `path`, and raw TOML `source`
- `workflows` — flat map of all workflows (root + children), keyed by resolved path
  - `source` — raw DOT source (unexpanded, pre-transform)
  - `config` — optional, the workflow's TOML config with `path` and `source`
  - `files` — map of normalized logical path (relative to that workflow's root directory) to file entry, each with `content` (file content) and `ref` (discovery metadata: `type`, `original` reference string, and optional `from` logical path for nested imports). Types: `file_inline` (`@file`), `import`, `dockerfile`. Note: `goal_file` no longer appears here — goals are in the top-level `goal` object

## Key Decisions

- **CLI gathers, server transforms.** The CLI's only job is reading files from disk and bundling them. All interpretation — TOML parsing, config merging, variable expansion, graph transforms, validation — happens server-side.
- **Detached/create flows keep CLI-allocated run IDs.** The manifest carries an optional `run_id`, preserving the current `run -d` / `create` behavior where the CLI can pre-generate the run ID before submission.
- **Flat workflow map.** All workflows (root and children, at any nesting depth) are in a single flat `workflows` map. Relationships are implicit via `stack.child_workflow` attributes in the DOT source. This avoids deep nesting and naturally deduplicates shared children.
- **Inline child workflows stay inline.** `stack.child_dot_source` continues to work exactly as it does today and does not need manifest bundling. Manifest child-workflow support is specifically for `stack.child_workflow` / `stack.child_dotfile`.
- **Child workflows don't have their own settings.** Today `parse_child_graph()` passes `Settings::default()` to children and never loads their TOML. The manifest preserves this — child workflow entries carry their DOT source and files but no separate config layers. If a child has a `workflow.toml`, it can optionally be included in `config` for future use, but the server does not merge it today.
- **Config resolution moves to the server.** The CLI currently merges `cli_args.combine(workflow_config).combine(project_config).combine(user_config).resolve()`. The manifest ships the raw layers and the server performs the merge. Merge precedence is determined by the server based on config `type`, not by array order.
- **Server merge precedence:** `args` > workflow `config` > `project` config > `user` config > server defaults. There is no separate manifest `env` layer. Server-owned operational settings such as `storage_dir`, `[server]`, `api`, `web`, `features`, `log`, and `exec` are ignored from manifest configs; the active server instance owns those.
- **File resolution must stay contextual.** Transforms currently resolve relative paths based on the current graph/file location. The manifest refactor cannot collapse that to `resolve("foo.md") -> content`; the resolver must accept the current logical directory so nested imports like `subflow/imported.fabro -> @prompts/foo.md` still resolve correctly.
- **Goal is resolved by the CLI and travels as a top-level object.** The CLI resolves the final goal using the current precedence rules (`--goal` / `--goal-file` over merged config `goal` / `goal_file`, otherwise graph-level `goal`) and sends it as `manifest.goal`. The server applies `manifest.goal.text` after config merge and clears `goal_file`, so goal handling never requires filesystem reads server-side.
- **Git state travels in the manifest.** The CLI captures origin URL (sanitized — credentials stripped from HTTPS URLs), current branch, commit SHA, and clean/dirty status. This replaces the server's need to run git commands or access the repo filesystem. Credential sanitization is mandatory to prevent token leakage in HTTPS URLs with embedded PATs or installation tokens.
- **`workflow_path` mode is removed.** After migration, the server only accepts manifests. The `dot_source` / `workflow_path` fields in `CreateRunRequest` are replaced by the manifest.
- **Preflight uses the same manifest.** `POST /api/v1/preflight` accepts a `RunManifest` and returns a `PreflightResponse` with workflow diagnostics plus the rendered checks payload. No validated manifest round-trip.
- **Manifest discovery walks the DOT AST.** The CLI must parse the DOT source enough to find `@file` references (in `prompt` and `goal` attributes), `import` attributes, and `stack.child_workflow` / `stack.child_dotfile` attributes. It does NOT run the full transform pipeline — just scans for file references.

## Implementation Changes

### 1. File resolver abstraction

Create `lib/crates/fabro-workflow/src/file_resolver.rs`.

```rust
pub trait FileResolver: Send + Sync {
    /// Resolve a logical reference string relative to the current logical directory.
    /// Returns the normalized logical path plus file content.
    fn resolve(&self, current_dir: &Path, reference: &str) -> Option<ResolvedFile>;
}

pub struct ResolvedFile {
    pub logical_path: PathBuf,
    pub content: String,
}
```

One implementation:

**`BundleFileResolver`** — reads from a manifest's files map:
```rust
pub struct BundleFileResolver {
    files: HashMap<PathBuf, String>,
}
```
The resolver normalizes `current_dir.join(reference)` into a workflow-relative logical path (strip leading `./`, collapse `.` / `..`) and looks up that normalized key. This preserves the current import/file-inlining semantics without any filesystem access. The `files` map is built from the manifest's `ManifestFileEntry` objects (extracting `content` by normalized logical path key).

No `DiskFileResolver` is needed — this is a hard cutover. The existing filesystem-based resolution logic in the transforms is replaced entirely.

Add `pub mod file_resolver;` to `lib/crates/fabro-workflow/src/lib.rs`.

Tests:
- `BundleFileResolver` with test data, verify exact key lookup works
- Verify `None` for missing files
- Verify path normalization handles `./` prefix stripping and nested `..`
- Verify nested import scoping resolves relative to the imported file's logical directory

### 2. Refactor FileInliningTransform

In `lib/crates/fabro-workflow/src/transforms/file_inlining.rs`:

Change the struct to hold a resolver instead of paths:
```rust
pub struct FileInliningTransform {
    resolver: Arc<dyn FileResolver>,
}

impl FileInliningTransform {
    pub fn new(resolver: Arc<dyn FileResolver>) -> Self {
        Self { resolver }
    }
}
```

Update `resolve_file_ref` to use the resolver:
- strip the `@` prefix to get the relative path
- call `self.resolver.resolve(current_dir, path_str)` instead of `std::fs::read_to_string`
- thread the current logical directory through the transform so imported files can inline their own relative references correctly
- remove tilde expansion and `canonicalize` logic (the CLI resolved all paths during bundling)

The `apply()` method stays structurally the same — it iterates node prompts and graph goal, calling the updated resolution logic.

### 3. Refactor ImportTransform

In `lib/crates/fabro-workflow/src/transforms/import.rs`:

Same pattern — hold a resolver:
```rust
pub struct ImportTransform {
    resolver: Arc<dyn FileResolver>,
}
```

Update `resolve_import_path` and `prepare_import`:
- `resolve_import_path` uses `self.resolver.resolve(current_dir, path_str)` instead of filesystem canonicalize
- `prepare_import` gets the file content from the resolver instead of `std::fs::read_to_string`
- when applying `FileInliningTransform` to imported content, pass the same resolver plus the imported file's logical parent directory

The recursive import expansion and circular import detection stay the same.

### 4. Update transform pipeline

In `lib/crates/fabro-workflow/src/pipeline/transform.rs`:

Change `TransformOptions` to carry a resolver:
```rust
pub struct TransformOptions {
    pub file_resolver: Option<Arc<dyn FileResolver>>,
    pub custom_transforms: Vec<Box<dyn Transform>>,
}
```

Update the `transform` function:
- where it currently checks `options.base_dir.is_some()` to gate `ImportTransform` and `FileInliningTransform`, check `options.file_resolver.is_some()` instead
- construct the transforms with the resolver

### 5. Manifest schema in OpenAPI

In `docs/api-reference/fabro-api.yaml`, add schemas:

```yaml
RunManifest:
  description: Self-contained workflow run manifest.
  type: object
  required:
    - version
    - cwd
    - target
    - workflows
  properties:
    version:
      type: integer
      description: Manifest schema version.
      example: 1
    run_id:
      type: string
      nullable: true
      description: Optional pre-generated run ID to use instead of allocating a new ULID.
      example: "01HV6D7S5YF4Z4B2M7K4N0Q6T9"
    cwd:
      type: string
      description: CLI working directory at invocation time.
    git:
      $ref: "#/components/schemas/ManifestGit"
    goal:
      $ref: "#/components/schemas/ManifestGoal"
    args:
      $ref: "#/components/schemas/ManifestArgs"
    target:
      $ref: "#/components/schemas/ManifestTarget"
    configs:
      type: array
      items:
        $ref: "#/components/schemas/ManifestConfig"
    workflows:
      type: object
      additionalProperties:
        $ref: "#/components/schemas/ManifestWorkflow"

ManifestGit:
  description: Observable git state from the CLI working directory.
  type: object
  required:
    - origin_url
    - branch
    - sha
    - clean
  properties:
    origin_url:
      type: string
      description: >
        Remote origin URL, sanitized (credentials stripped from HTTPS URLs).
        e.g. https://user:token@github.com/acme/app.git becomes https://github.com/acme/app.git
      example: "https://github.com/acme/my-app.git"
    branch:
      type: string
      description: Current branch name.
      example: feature/foo
    sha:
      type: string
      description: Current commit SHA.
      example: abc123def
    clean:
      type: boolean
      description: Whether the working tree has uncommitted changes.

ManifestGoal:
  description: Resolved goal with provenance.
  type: object
  required:
    - type
    - text
  properties:
    type:
      type: string
      enum:
        - value
        - file
        - graph
      description: >
        How the goal was sourced: "value" (literal from TOML goal field or --goal flag),
        "file" (resolved from goal_file), "graph" (from graph-level goal attribute in DOT).
    text:
      type: string
      description: The resolved goal content. Server uses this directly.
    path:
      type: string
      description: Original file path (only present when type is "file").

ManifestTarget:
  type: object
  required:
    - identifier
    - path
  properties:
    identifier:
      type: string
      description: What the user typed (slug or path).
      example: smoke
    path:
      type: string
      description: Resolved path, keys into the workflows map.
      example: fabro/workflows/smoke/workflow.fabro

ManifestConfig:
  type: object
  required:
    - type
  properties:
    type:
      type: string
      enum:
        - project
        - user
    path:
      type: string
      description: Filesystem path to the config file.
    source:
      type: string
      description: Raw TOML source of the config file.

ManifestWorkflowConfig:
  type: object
  required:
    - path
    - source
  properties:
    path:
      type: string
      description: Path to the workflow TOML file.
    source:
      type: string
      description: Raw TOML source.

ManifestArgs:
  description: Command-local run/preflight flags that affect run settings. All fields optional (sparse).
  type: object
  properties:
    model:
      type: string
    provider:
      type: string
    sandbox:
      type: string
    verbose:
      type: boolean
    dry_run:
      type: boolean
    auto_approve:
      type: boolean
    no_retro:
      type: boolean
    preserve_sandbox:
      type: boolean
    label:
      type: array
      items:
        type: string

ManifestFileEntry:
  description: A bundled file with discovery metadata.
  type: object
  required:
    - content
    - ref
  properties:
    content:
      type: string
      description: File content.
    ref:
      $ref: "#/components/schemas/ManifestFileRef"

ManifestFileRef:
  description: How this file was discovered.
  type: object
  required:
    - type
    - original
  properties:
    type:
      type: string
      enum:
        - file_inline
        - import
        - dockerfile
      description: Discovery type.
    original:
      type: string
      description: The reference string as it appeared in the DOT/TOML.
      example: "@prompts/review.md"
    from:
      type: string
      description: Optional logical path of the file/graph that referenced this entry.

ManifestWorkflow:
  type: object
  required:
    - source
  properties:
    source:
      type: string
      description: Raw DOT source (unexpanded, pre-transform).
    config:
      $ref: "#/components/schemas/ManifestWorkflowConfig"
    files:
      type: object
      additionalProperties:
        $ref: "#/components/schemas/ManifestFileEntry"
      description: >
        Map of normalized logical path to file entry with content and discovery metadata.
```

Replace the `CreateRunRequest` schema with `RunManifest` on `POST /api/v1/runs`.

Add `POST /api/v1/preflight`:
```yaml
/api/v1/preflight:
  post:
    operationId: runPreflight
    tags: [Runs]
    summary: Validate a workflow manifest without creating a run.
    description: >
      Accepts the same manifest as POST /runs. Validates the workflow,
      checks sandbox availability, LLM provider access, and GitHub token
      minting. Returns a structured pass/fail report.
    requestBody:
      required: true
      content:
        application/json:
          schema:
            $ref: "#/components/schemas/RunManifest"
    responses:
      "200":
        description: Preflight report.
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/PreflightResponse"
```

The preflight response should preserve the current CLI JSON contract:
- `workflow` summary/diagnostics from validation
- `checks` as the rendered report payload

So add:
```yaml
PreflightResponse:
  type: object
  required:
    - workflow
    - checks
  properties:
    workflow:
      $ref: "#/components/schemas/PreflightWorkflowSummary"
    checks:
      $ref: "#/components/schemas/DiagnosticsReport"
```

Rebuild `fabro-api`:
```
cargo build -p fabro-api
```

### 6. Server-side config resolution from manifest

Create `lib/crates/fabro-server/src/manifest.rs`.

This module:

1. **Parses run-relevant config layers from the manifest:**
   - `args` → converted to a `ConfigLayer` (map CLI arg names to `ConfigLayer` fields, similar to `TryFrom<&RunArgs>` in `overrides.rs`)
   - workflow `config.source` → parsed as TOML via `fabro_config` and converted to `ConfigLayer`
   - each `configs[]` entry → parsed as TOML and converted to `ConfigLayer`
   - strip or ignore non-run fields from uploaded configs (`storage_dir`, `[server]`, `api`, `web`, `features`, `log`, `exec`, `max_concurrent_runs`)

2. **Merges in server-determined precedence:**
   ```rust
   pub fn resolve_settings(manifest: &RunManifest, server_defaults: &Settings) -> Result<Settings> {
       let args_layer = parse_args_layer(&manifest.args)?;
       let workflow_layer = parse_workflow_config(manifest)?;
       let project_layer = find_config_layer(manifest, "project")?;
       let user_layer = find_config_layer(manifest, "user")?;

       args_layer
           .combine(workflow_layer)
           .combine(project_layer)
           .combine(user_layer)
           .resolve()
   }
   ```

3. **Applies the top-level goal after merge.**
   - if `manifest.goal` is present, set `settings.goal = Some(manifest.goal.text.clone())`
   - clear `settings.goal_file` so server-side goal handling never tries to read the filesystem

4. **Builds a `BundleFileResolver`** from the target workflow's `files` map.

5. **Constructs a `CreateRunInput`** with:
   - `WorkflowInput::DotSource { source, base_dir: None }` — using the raw DOT from the manifest
   - resolved `Settings`
   - `cwd` from the manifest
   - a `file_resolver` for the transform pipeline

The `parse_args_layer` function maps manifest `args` keys to `ConfigLayer` fields. The mapping mirrors the current `TryFrom<&RunArgs>` / `TryFrom<&PreflightArgs>` logic in `overrides.rs`, but without `goal` / `goal_file` because those are represented by the top-level `goal` object. The keys are the same field names: `model`, `provider`, `sandbox`, `verbose`, `dry_run`, `auto_approve`, `no_retro`, `preserve_sandbox`, `label`.

### 7. Update POST /runs handler

In `lib/crates/fabro-server/src/server.rs`:

Replace the current `create_run` handler. The new handler:

1. Deserializes the request body as `RunManifest`.
2. Validates manifest version is supported.
3. Calls `manifest::resolve_settings(&manifest, &state.settings)` to merge configs.
4. Looks up the root workflow in `manifest.workflows` using `manifest.target.path`.
5. Builds a `BundleFileResolver` from the root workflow's `files` map.
6. Parses `manifest.run_id` when present, preserving the current detached/local create behavior.
7. Constructs `CreateRunInput`:
   ```rust
   CreateRunInput {
       workflow: WorkflowInput::DotSource {
           source: root_workflow.source.clone(),
           base_dir: None,
       },
       settings,
       cwd: PathBuf::from(&manifest.cwd),
       workflow_slug: Some(manifest.target.identifier.clone()),
       run_id: Some(run_id),
       host_repo_path: None,
       base_branch: None,
   }
   ```
8. Passes the manifest's workflow map to `operations::create()` so child workflows can be resolved later.

The `operations::create()` and `validate()` paths in `fabro-workflow` need to accept the file resolver (via `TransformOptions`) and the workflow map (for child resolution). This requires updating `CreateRunInput`, `ValidateInput`, or shared workflow-resolution state:

```rust
pub struct CreateRunInput {
    pub workflow: WorkflowInput,
    pub settings: Settings,
    pub cwd: PathBuf,
    pub workflow_slug: Option<String>,
    pub run_id: Option<RunId>,
    pub host_repo_path: Option<String>,
    pub base_branch: Option<String>,
    pub file_resolver: Option<Arc<dyn FileResolver>>,
    pub workflow_bundle: Option<HashMap<String, ManifestWorkflow>>,
}
```

In `operations::create()` (`create.rs`) and `validate()` (`validate.rs`), when `file_resolver` is `Some`, use it in `TransformOptions` instead of relying on `base_dir`.

### 8. Child workflow resolution from manifest

In `lib/crates/fabro-workflow/src/handler/manager_loop.rs`:

`parse_child_graph()` currently resolves `stack.child_workflow` as `WorkflowInput::Path` and reads from disk. Update it to check for a workflow bundle first:

```rust
fn parse_child_graph(node: &Node, services: &EngineServices) -> Result<...> {
    // ... existing stack.child_dot_source handling ...

    if let Some(path) = node.attr("stack.child_workflow").or(node.attr("stack.child_dotfile")) {
        let bundle = services.workflow_bundle.as_ref()
            .ok_or_else(|| anyhow!("no workflow bundle available"))?;
        let child = bundle.get(path)
            .ok_or_else(|| anyhow!("child workflow not found in manifest: {path}"))?;
        let resolver = BundleFileResolver::new(child.files.clone());
        // Pass resolver plus the child's logical root to validate()
        Ok(WorkflowInput::DotSource {
            source: child.source.clone(),
            base_dir: None,
        })
    }
}
```

Add `workflow_bundle: Option<Arc<HashMap<String, ManifestWorkflow>>>` to `EngineServices` so it is accessible during execution.

When the child workflow is resolved from the bundle, its own `files` map provides a scoped `BundleFileResolver` for that child's transforms (`@file` refs, imports within the child).

### 9. CLI manifest builder

Create `lib/crates/fabro-cli/src/manifest_builder.rs`.

This module builds a `RunManifest` from CLI inputs:

```rust
pub struct ManifestBuilder;

impl ManifestBuilder {
    pub fn build_for_run(cwd: PathBuf, args: &RunArgs) -> Result<RunManifest> { ... }
    pub fn build_for_preflight(cwd: PathBuf, args: &PreflightArgs) -> Result<RunManifest> { ... }
}
```

The build process:

1. **Resolve workflow path**: call `project_config::resolve_workflow_path(&args.workflow, &cwd)` to get the `.fabro` file path and optional `.toml` config path.

2. **Read the root workflow**:
   - read the `.fabro` file: `std::fs::read_to_string(&dot_path)`
   - if a `.toml` exists, read it: `std::fs::read_to_string(&toml_path)`

3. **Discover file references in the DOT source**:
   - parse the DOT source with `parser::parse(&source)`
   - scan all nodes for `prompt` attributes starting with `@` → collect file paths
   - scan graph-level `goal` attribute for `@` prefix → collect file path
   - scan all nodes for `import` attributes → collect file paths
   - scan all nodes for `stack.child_workflow` / `stack.child_dotfile` attributes → collect child workflow paths

4. **Resolve the final goal**:
   - compute the final goal using the current precedence rules (`--goal` / `--goal-file` over merged config `goal` / `goal_file`, otherwise graph-level `goal`)
   - store it in top-level `manifest.goal`
   - do **not** add `goal_file` to the workflow `files` map

5. **Resolve file references from the TOML config**:
   - if the TOML has `sandbox.daytona.snapshot.dockerfile.path`, read the Dockerfile and add to `files`

6. **Read all discovered files** into the `files` map, keyed by normalized logical path relative to the workflow root. Resolve relative to the `.fabro` file's parent directory, with `~/.fabro` as fallback (matching current `resolve_file_ref` logic).

7. **Recursively process child workflows** (step 3-6 for each child). Children go into the flat `workflows` map. Detect circular references via a visited set.

8. **Process imported `.fabro` files**: imports also go into the workflow's `files` map (they are read and their content is stored under workflow-relative logical paths). Imported files may themselves contain `@file` refs and nested imports — the builder must recursively discover these too.

9. **Gather configs**:
   - read `fabro.toml` via `project_config::discover_project_config()`
   - read `~/.fabro/user.toml` via the user config path
   - for each, record `type`, `path`, and raw `source`

10. **Gather args**: serialize the command-local args that affect run settings. Use the same field names as `ConfigLayer` (`model`, `provider`, `sandbox`, etc.) but exclude `goal` / `goal_file` because those are represented by top-level `manifest.goal`. Only include fields that the user actually set (sparse).

11. **Carry the optional run ID** from `RunArgs.run_id` when present.

12. **Assemble and return** the `RunManifest`.

The DOT parser is already available in `fabro-workflow`. The CLI already depends on `fabro-workflow` (it calls `validate()`). The manifest builder reuses the parser for discovery but does NOT run transforms.

### 10. Update CLI commands

In `lib/crates/fabro-cli/src/commands/run/create.rs`:

Replace the current flow:
```rust
// Before (sends path + pre-resolved settings):
let settings = cli_args_config.combine(workflow_config).combine(cli_defaults).resolve()?;
client.create_run_from_workflow_path(workflow_path, &cwd, &settings, run_id)

// After (sends manifest):
let manifest = ManifestBuilder::build_for_run(cwd, &args)?;
client.create_run_from_manifest(&manifest)
```

Remove `create_run_from_workflow_path` from `server_client.rs`. Add `create_run_from_manifest` that POSTs the manifest JSON.

The optional local validation step (lines 39-51) can be removed — the server validates as part of run creation. Or it can stay as a fast-fail with a note that it won't catch everything the server checks.

In `lib/crates/fabro-cli/src/commands/run/overrides.rs`:

The `TryFrom<&RunArgs> for ConfigLayer` conversion is no longer needed for the settings merge (the server does it). But the args serialization for the manifest's `args` field needs similar logic. Consider:
- keeping the conversion as a helper for building the manifest's `args` object
- or writing a new `RunArgs::to_manifest_args()` method that produces a JSON map

In `lib/crates/fabro-cli/src/commands/preflight.rs`:

Replace the current flow:
```rust
// Before (runs all checks CLI-side):
let settings = cli_args_config.combine(workflow_config).combine(cli_defaults).resolve()?;
validate(ValidateInput { ... })?;
run_preflight(&settings, ...)?;

// After (sends manifest to server):
let manifest = ManifestBuilder::build_for_preflight(cwd, &args)?;
let cli_settings = load_user_settings_with_storage_dir(args.storage_dir.as_deref())?;
let client = server_client::connect_server(&cli_settings.storage_dir()).await?;
let response = client.run_preflight().body(manifest).send().await?;
render_report(&response.checks);
```

Remove all local preflight check functions. The CLI becomes a thin client that builds the manifest, sends it, and renders the response.

### 11. Preflight handler on server

In `lib/crates/fabro-server/src/server.rs`:

```rust
async fn run_preflight(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(manifest): Json<RunManifest>,
) -> Response {
    let report = preflight::run_preflight(&state, &manifest).await;
    (StatusCode::OK, Json(report)).into_response()
}
```

Create `lib/crates/fabro-server/src/preflight.rs`.

This module adapts the checks from the current CLI-side `preflight.rs`:

1. **Resolve settings** from manifest via `manifest::resolve_settings()`.
2. **Validate the workflow** — parse, transform (using `BundleFileResolver`), validate.
3. **Check sandbox** — resolve sandbox provider from settings, attempt to create/initialize a test sandbox.
4. **Check LLM providers** — for each model used by the graph, verify the provider is configured (secret exists in secret store) and reachable.
5. **Check GitHub token** — if the workflow has `github.permissions`, attempt to mint a test installation access token.

Each check produces a `CheckResult`. The function returns a `PreflightResponse` with:
- `workflow` summary (`name`, node/edge counts, goal, diagnostics)
- `checks` as the `DiagnosticsReport` payload

Add route in `real_routes()`:
```rust
.route("/preflight", post(run_preflight))
```

### 12. Demo mode

In `lib/crates/fabro-server/src/demo/mod.rs`:

**Run creation demo**: The demo handler already exists for `POST /runs`. Update it to accept the manifest schema. The demo can ignore the manifest contents and return a canned run response as it does today.

**Preflight demo**: Return an all-passing preflight report:
```rust
pub(crate) async fn run_preflight(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Json(_manifest): Json<RunManifest>,
) -> Response {
    (StatusCode::OK, Json(serde_json::json!({
        "workflow": {
            "name": "demo",
            "nodes": 3,
            "edges": 2,
            "goal": "Demo",
            "diagnostics": []
        },
        "checks": {
            "version": fabro_util::version::FABRO_VERSION,
            "sections": [
                {
                    "title": "Workflow",
                    "checks": [
                        { "name": "Parse & Validate", "status": "pass", "summary": "3 nodes, 2 edges, goal set", "details": [] },
                    ]
                },
                {
                    "title": "Sandbox",
                    "checks": [
                        { "name": "Provider", "status": "pass", "summary": "local sandbox available", "details": [] },
                    ]
                },
                {
                    "title": "LLM",
                    "checks": [
                        { "name": "Providers", "status": "pass", "summary": "Anthropic, OpenAI reachable", "details": [] },
                    ]
                },
            ]
        }
    }))).into_response()
}
```

Wire in `demo_routes()`:
```rust
.route("/preflight", post(demo::run_preflight))
```

### 13. Cleanup

After the manifest is working:

- **Remove `workflow_path` mode** from the `POST /runs` handler. Remove the `workflow_path`, `cwd`, `settings_json` fields from the request schema. Remove `CreateRunRequest` from the OpenAPI spec and replace with `RunManifest`.
- **Keep `WorkflowInput::Path` for local-only CLI commands.** Do not remove it from `source.rs`; commands like `validate` and `graph` still use local path resolution. The cleanup is server-specific: remove the server's path-based submission path, not the workflow crate's local path input abstraction.
- **No `DiskFileResolver` to remove** — it was never created (hard cutover).
- **Remove `create_run_from_workflow_path`** from `server_client.rs`.
- **Remove `ConfigLayer::for_workflow()`** usage from CLI run/preflight commands (the server does config resolution now). The function itself may still be useful for other CLI code paths.
- **Remove CLI-side preflight check functions** from `preflight.rs`.
- **Remove `TryFrom<&RunArgs> for ConfigLayer`** if replaced by manifest args serialization.

## Implementation Order

```
 1  File resolver trait + BundleFileResolver                     (no deps)
 2  Refactor FileInliningTransform to use resolver                (depends on 1)
 3  Refactor ImportTransform to use resolver                      (depends on 1)
 4  Update transform pipeline (TransformOptions)                  (depends on 2, 3)
 5  Manifest schema in OpenAPI + rebuild fabro-api                (no deps, parallel with 1-4)
 6  Server-side config resolution (manifest.rs)                   (depends on 5)
 7  CLI manifest builder                                          (depends on 5)
 8  Update POST /runs handler to accept manifest                  (depends on 4, 6)
 9  Child workflow resolution from manifest                       (depends on 8)
10  Update CLI run/create to send manifest                        (depends on 7, 8)
11  Preflight server handler                                      (depends on 4, 6)
12  Update CLI preflight to send manifest                         (depends on 7, 11)
13  Demo mode                                                     (depends on 5)
14  Cleanup: remove workflow_path mode + dead code                (depends on 10, 12)
```

Steps 1-4 (resolver refactor) and 5 (schema) can proceed in parallel. Step 7 (CLI builder) and 6 (server config) can proceed in parallel once the schema exists.

## Resolved Questions

1. **`args` field schema**: **Typed and limited to run settings.** The OpenAPI schema defines explicit fields matching the command-local run/preflight overrides (model, provider, sandbox, verbose, dry_run, auto_approve, no_retro, preserve_sandbox, label). `goal` / `goal_file` are excluded because the final goal is represented by top-level `manifest.goal`. All fields are optional (sparse — only set fields are present).

2. **Import path scoping**: **Workflow-relative logical paths plus contextual resolution.** All file paths in a workflow's `files` map are normalized logical paths relative to that workflow's root directory. Each file entry carries `ref` metadata with `type`, `original`, and optional `from`. The server resolves nested imports and `@file` references by passing the current logical directory into the resolver; it does not use metadata fields for lookup.

3. **Backward compatibility**: **Hard cutover.** The old `CreateRunRequest` (workflow_path/dot_source) is removed when the manifest lands. CLI and local server are the same binary, so they upgrade together. Remote servers need coordinated upgrade.

4. **DOT parser for discovery**: **Full parse.** The CLI uses the existing `fabro-workflow` parser (already a dependency). It's fast, handles all edge cases (quoted strings, comments, escapes), and is more reliable than regex scanning.

5. **No manifest `env` layer**: the current CLI does not have a separate run-settings env layer like `FABRO_MODEL` or `FABRO_PROVIDER`. Manifest config precedence is `args` > workflow config > project config > user config > server defaults, with server-owned operational settings stripped from uploaded configs.
