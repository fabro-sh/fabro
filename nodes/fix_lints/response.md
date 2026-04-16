Both clippy lint warnings are now fixed:

1. **`lib/crates/fabro-server/src/serve.rs:504`** — Added `#[expect(clippy::disallowed_methods, reason = "Intentional synchronous spawn before entering the async server loop")]` to the `std::process::Command::new("bun")` call. This is an intentional synchronous subprocess spawn that happens before entering the async server loop, so using the synchronous `std::process::Command` rather than `tokio::process::Command` is correct here.

2. **`lib/crates/fabro-cli/src/commands/install.rs:161`** — Replaced the absolute path `fabro_server::serve::DEFAULT_TCP_PORT` with a proper `use` import at the top of the file, satisfying the `clippy::absolute_paths` lint.