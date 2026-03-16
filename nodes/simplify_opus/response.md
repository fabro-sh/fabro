All three review agents completed and found **no issues**. The change is clean:

- **Code Reuse**: No duplication — the 4-line quiet loop is a minimal, context-specific idiom with no existing utility to replace it.
- **Code Quality**: The `quiet` flag is a natural peer to the existing `json` and `all` flags, correctly placed before the JSON branch so it takes precedence, following the established `docker ps` convention.
- **Efficiency**: `scan_runs` reads full metadata even in quiet mode, but this is correct — filtering requires the full `RunInfo`, and this is a CLI command (not a hot path).

No fixes needed. The code is already clean.