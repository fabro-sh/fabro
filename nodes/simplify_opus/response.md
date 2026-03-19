Here's a summary of the review and what was fixed:

## Review Summary

### Issue Found: Duplicated OAuth Flow (Copy-paste with slight variation)

The OpenAI OAuth browser flow — run browser auth, handle success/failure, fall back to API key on error — was **duplicated** across two call sites:
- `commands/provider.rs` lines 29-65 (new code)  
- `install.rs` lines 562-599 (existing code that was not refactored during the extraction)

Both had identical logic: print "Opening browser...", call `run_browser_flow()`, extract account_id on success, fall back to `prompt_and_validate_key()` on failure.

### Fix Applied

Extracted a new shared helper `run_openai_oauth_or_api_key()` in `provider_auth.rs` that encapsulates the full "try OAuth, fall back to API key" flow. Both callers now use it:

- **`commands/provider.rs`**: Collapsed from 37 lines of OAuth logic to a single call (`provider_auth::run_openai_oauth_or_api_key(&s).await?`)
- **`install.rs`**: Collapsed from 25 lines of OAuth logic to 4 lines (call + extend env_pairs + push provider + set flag)

**Net result**: -25 lines, single source of truth for the OAuth flow.

### Other Findings (not worth fixing)

- **`provider_display_name`** is not redundant — `Provider::Display` outputs lowercase ("openai"), while this returns title case ("OpenAI") for UI display
- **`~/.fabro` dir pattern** is repeated everywhere in the codebase, but that's a pre-existing issue not introduced by this change
- **`std::env::set_var` in `validate_api_key`** is process-global mutation, but this was moved as-is from `install.rs` and only runs in the interactive CLI context (single-threaded validation flow)
- **The `refs` Vec in `write_env_file`** converts `&[(String, String)]` to `Vec<(&str, &str)>` for the `merge_env` API — necessary for type compatibility