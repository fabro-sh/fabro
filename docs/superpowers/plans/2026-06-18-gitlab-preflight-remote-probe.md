# GitLab Preflight Remote Probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make clone-based sandbox preflight verify GitLab repository and branch access with the same `git ls-remote` remote probe used for GitHub.

**Architecture:** Keep the existing `run_manifest.rs` preflight flow, but stop treating configured GitLab origins as automatically reachable. Add GitLab auth metadata to the remote-ref check request, pass it to `git ls-remote` as an `http.extraHeader`, and redact the header/token from failure output. The user-facing check result should remain the existing `Repository Access` pass/error result.

**Tech Stack:** Rust, Tokio, `git ls-remote`, `fabro_gitlab::basic_auth_header_value`, existing `cargo nextest` tests.

---

## File Structure

- Modify `lib/crates/fabro-server/src/run_manifest.rs`
  - Extend `GitRemoteRefCheck` with an optional extra HTTP header for authenticated Git remotes.
  - Change GitLab origins to build and execute a real remote-ref check instead of returning an immediate pass.
  - Add redaction for GitLab token/header in failed `git ls-remote` output.
  - Replace the old “without remote probe” test with tests that prove GitLab probes and surfaces failures.

## Task 1: Prove GitLab Preflight Uses The Remote Probe

**Files:**
- Modify: `lib/crates/fabro-server/src/run_manifest.rs`
- Test: `lib/crates/fabro-server/src/run_manifest.rs`

- [ ] **Step 1: Replace the old GitLab skip test with a failing probe test**

Replace `repository_access_check_accepts_configured_gitlab_origin_without_remote_probe` with:

```rust
#[tokio::test]
async fn repository_access_check_probes_configured_gitlab_branch() {
    let (prepared, resolved) = prepared_and_resolved_for_sandbox(
        SandboxProviderKind::Docker,
        true,
        Some(git_context("https://gitlab.com/acme/widgets", "main")),
    );
    let gitlab = fabro_sandbox::GitLabSandboxConfig {
        base_url:    fabro_gitlab::repository::GitLabBaseUrl::parse("https://gitlab.com")
            .unwrap(),
        credentials: fabro_gitlab::GitLabCredentials::Token("glpat-test".to_string()),
    };
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls_for_check = Arc::clone(&calls);
    let mut checks = Vec::new();

    let ok = run_repository_access_check_with(
        &mut checks,
        SandboxProviderKind::Docker,
        &prepared,
        &resolved,
        None,
        Some(&gitlab),
        move |request, _github_app| {
            calls_for_check.lock().unwrap().push(request);
            async { Ok(()) }
        },
    )
    .await;

    assert!(ok);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].origin_url, "https://gitlab.com/acme/widgets");
    assert_eq!(calls[0].branch.as_deref(), Some("main"));
    assert_eq!(
        calls[0].extra_header.as_deref(),
        Some("Authorization: Basic b2F1dGgyOmdscGF0LXRlc3Q=")
    );
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "Repository Access");
    assert_eq!(checks[0].status, CheckStatus::Pass);
    assert_eq!(checks[0].summary, "reachable");
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run:

```bash
cargo nextest run -p fabro-server repository_access_check_probes_configured_gitlab_branch
```

Expected: FAIL because `GitRemoteRefCheck` does not have `extra_header` yet and GitLab still skips the probe.

- [ ] **Step 3: Extend the remote-ref request with an optional extra header**

Change `GitRemoteRefCheck` to:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
struct GitRemoteRefCheck {
    origin_url:   String,
    branch:       Option<String>,
    extra_header: Option<String>,
}
```

Update existing GitHub test expectations to include `extra_header: None`.

- [ ] **Step 4: Route GitLab origins into the shared probe**

In `run_repository_access_check_with`, replace the immediate GitLab pass branch with construction of:

```rust
let request = GitRemoteRefCheck {
    origin_url: repo.clean_origin_url,
    branch: Some(git.branch.clone()).filter(|branch| !branch.trim().is_empty()),
    extra_header: Some(format!(
        "Authorization: {}",
        fabro_gitlab::basic_auth_header_value(token)
    )),
};
```

Then call the existing `check_remote_ref` closure and emit the same pass/error result shape used by GitHub.

- [ ] **Step 5: Run the focused test and verify it passes**

Run:

```bash
cargo nextest run -p fabro-server repository_access_check_probes_configured_gitlab_branch
```

Expected: PASS.

## Task 2: Prove GitLab Remote Probe Failures Fail Preflight

**Files:**
- Modify: `lib/crates/fabro-server/src/run_manifest.rs`
- Test: `lib/crates/fabro-server/src/run_manifest.rs`

- [ ] **Step 1: Add a GitLab failure test**

Add:

```rust
#[tokio::test]
async fn repository_access_check_surfaces_gitlab_remote_probe_failure() {
    let (prepared, resolved) = prepared_and_resolved_for_sandbox(
        SandboxProviderKind::Docker,
        true,
        Some(git_context("https://gitlab.com/acme/widgets", "missing")),
    );
    let gitlab = fabro_sandbox::GitLabSandboxConfig {
        base_url:    fabro_gitlab::repository::GitLabBaseUrl::parse("https://gitlab.com")
            .unwrap(),
        credentials: fabro_gitlab::GitLabCredentials::Token("glpat-test".to_string()),
    };
    let mut checks = Vec::new();

    let ok = run_repository_access_check_with(
        &mut checks,
        SandboxProviderKind::Docker,
        &prepared,
        &resolved,
        None,
        Some(&gitlab),
        |_request, _github_app| async { Err("GitLab branch not found".to_string()) },
    )
    .await;

    assert!(!ok);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "Repository Access");
    assert_eq!(checks[0].status, CheckStatus::Error);
    assert_eq!(checks[0].summary, "failed");
    assert!(
        checks[0]
            .remediation
            .as_deref()
            .unwrap_or_default()
            .contains("GitLab branch not found")
    );
}
```

- [ ] **Step 2: Run the focused test and verify it passes after Task 1**

Run:

```bash
cargo nextest run -p fabro-server repository_access_check_surfaces_gitlab_remote_probe_failure
```

Expected: PASS.

## Task 3: Make The Real `git ls-remote` Command Use And Redact The GitLab Header

**Files:**
- Modify: `lib/crates/fabro-server/src/run_manifest.rs`
- Test: `lib/crates/fabro-server/src/run_manifest.rs`

- [ ] **Step 1: Add `extra_header` support to `check_git_remote_ref`**

Before the `ls-remote` args, add:

```rust
if let Some(header) = request.extra_header.as_ref() {
    command.args(["-c", &format!("http.extraHeader={header}")]);
}
```

Then keep the existing `git ls-remote --heads --exit-code ...` arguments.

- [ ] **Step 2: Redact the GitLab header from command failures**

After the existing `redact_auth_url` call, redact the extra header value:

```rust
let mut message = redact_auth_url(&message, auth_url.as_ref());
if let Some(header) = request.extra_header.as_ref() {
    message = message.replace(header, "<redacted>");
}
Err(message)
```

- [ ] **Step 3: Run affected tests**

Run:

```bash
cargo nextest run -p fabro-server repository_access_check
cargo nextest run -p fabro-server run_manifest
```

Expected: PASS.

## Self-Review

- Spec coverage: The plan replaces the GitLab preflight false-positive with the same remote probe path GitHub uses, including branch checks and failure propagation.
- Placeholder scan: No placeholder implementation steps remain.
- Type consistency: The `extra_header` field is introduced before all tests and production code use it.
