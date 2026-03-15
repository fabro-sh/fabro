Here's a summary of what was reviewed and fixed:

## Review Summary

### Findings Addressed

1. **Duplicated `perm_details` construction (copy-paste)** — In `run_preflight()`, the permission details Vec (`gh_cfg.permissions.iter().map(|(k, v)| CheckDetail::new(...)).collect()`) was constructed identically in both the `Ok(_)` and `Err(e)` branches of the token minting result. **Fixed:** Extracted `perm_details` before the match and used it in both branches.

2. **Unnecessary early clone of `github_app`** — `github_app_for_preflight = github_app.clone()` was done eagerly before the sandbox match, even though `github_app` is only moved in the `Daytona` branch. **Fixed:** Removed the pre-clone, moved `github_app.clone()` to the Daytona branch (the only place that consumes it), and used `&github_app` directly for the preflight check.

### Reviewed but No Action Needed

- **`reqwest::Client::new()` in `mint_github_token`** — The existing `create_pull_request` in `fabro-github` follows the same pattern (creates a new client per call). This is consistent with the codebase convention and token minting is not a hot path.
- **Duplicated permission resolution pattern** (`run_cfg.and_then(|c| c.github.as_ref()).or(run_defaults.github.as_ref())`) — Appears in both `run_command` and `run_preflight`, but this matches how other config fields (mcp_servers, hooks, etc.) are resolved inline. Not worth extracting for two call sites in different functions.
- **Stringly-typed permissions** (`HashMap<String, String>`) — GitHub's permission model is open-ended and user-declared; a string map is the right representation here, matching how the GitHub API accepts permissions.
- **Config/test boilerplate** (`github: None` additions) — These are necessary structural additions when a new field is added to a struct. Clean and minimal.