Here's a summary of what was fixed:

1. **Fixed compilation error in `install.rs`**: The simplify agent changed `setup_github_app`'s return type to `(String, Vec<(String, String)>)` but didn't update the function body to return the slug. The caller also didn't expect a tuple. Fixed by restoring the return type to `Result<Vec<(String, String)>>` which matches both the body and the caller.

2. **Eliminated copy-paste in `commands/provider.rs`**: The `vec![(env_var, key)]` pattern appeared in two branches (OpenAI-declined-OAuth and non-OpenAI). Simplified by hoisting the OAuth check into a single `use_oauth` boolean using short-circuit `&&`, then having a clean if/else with the API key path appearing only once. This reduced the file from 43 to 37 lines.

The rest of the code was already clean — no reuse issues (no existing `display_name()` on `Provider`, no shared `~/.fabro` dir helper), no efficiency problems, and no other quality concerns.