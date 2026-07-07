# rustfmt and Formatting

## Rule

Use the checked-in `rustfmt.toml` as the formatting authority and run rustfmt with the pinned nightly toolchain.

## Why

Formatting should be mechanical and reproducible. A pinned rustfmt version prevents agents, editors, and CI from producing different diffs when the project uses unstable rustfmt options.

## Do

- Check in `rustfmt.toml` at the workspace root.
- Use `nightly-2026-04-14` for formatting.
- Run `cargo +nightly-2026-04-14 fmt --all` before committing Rust changes.
- Run `cargo +nightly-2026-04-14 fmt --check --all` in CI.
- Keep editor, agent, and CI commands aligned with the same pinned toolchain.
- Let rustfmt decide layout instead of hand-formatting around it.

## Avoid

- Do not run unpinned `cargo fmt` when the project has this config.
- Do not manually preserve formatting that rustfmt changes.
- Do not mix stable rustfmt and pinned nightly rustfmt in the same repository.
- Do not change formatting settings as part of unrelated feature work.
- Do not use `#[rustfmt::skip]` except for generated code or unusual literals where formatting would damage readability.

## Example

Run the checked-in formatter configuration:

```sh
cargo +nightly-2026-04-14 fmt --all
cargo +nightly-2026-04-14 fmt --check --all
```

Use the new project workflow for the initial `rustfmt.toml` contents.

## Exceptions

- Existing projects may keep their current rustfmt pin until a focused formatting update.
- Generated code may opt out of formatting when regeneration controls the file layout.
- Public examples may use manual line breaks when rustfmt does not run on the snippet.
