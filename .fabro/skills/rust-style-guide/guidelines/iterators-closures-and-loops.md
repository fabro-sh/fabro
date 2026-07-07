# Iterators, Closures, and Loops

## Rule

Use iterator chains for simple transformations and loops for branching, mutation, early exits, or multi-step logic; treat Clippy as authoritative for local iterator-vs-loop idioms.

## Why

Iterator chains are compact when they read as a pipeline. Loops are clearer when the code carries state, exits early, performs side effects, or needs named intermediate steps.

## Do

- Use `.iter()`, `.iter_mut()`, and `.into_iter()` intentionally based on whether the code borrows, mutates, or consumes values.
- Use `map`, `filter`, `filter_map`, `flat_map`, `find`, `any`, `all`, and `position` when they directly name the operation.
- Use `collect` when the target collection is clear; add a type annotation when inference makes the result hard to see.
- Collect fallible maps with `collect::<Result<Vec<_>, _>>()` (or the `Option` equivalent) to fail fast on the first error; reserve `try_fold` for accumulation that carries state.
- Use `try_fold` or `try_for_each` for short fallible accumulation or validation when it stays readable.
- Use `for` loops for branching, mutation, early `break`/`continue`, multiple accumulators, or nontrivial error handling.
- Keep closures short; extract a named helper when a closure has branching, side effects, or reused logic.
- Use `move` closures when a closure outlives the current scope, is spawned, or ownership is clearer than borrowing.
- Clone into closures when that avoids awkward lifetimes and the cost is not known to matter.
- Prefer `enumerate` and `zip` over manual index tracking when pairing is direct.

## Avoid

- Do not write long iterator chains that hide control flow.
- Do not use `for_each` for side-effect-heavy loops when a `for` loop is clearer.
- Do not use `fold` with a complex mutable accumulator when a loop communicates the state better.
- Do not `collect` into a temporary collection only to iterate over it once.
- Do not hide logging, metrics, mutation, or I/O inside `map` or `filter` closures.
- Do not rely on dense closure inference when a named helper or local type annotation would clarify intent.

## Example

Use an iterator pipeline for simple extraction:

```rust
pub fn active_names(runs: &[Run]) -> Vec<String> {
    runs.iter()
        .filter(|run| run.is_active())
        .map(|run| run.name().to_owned())
        .collect()
}
```

Use a loop when the code branches, accumulates state, and can fail:

```rust
pub fn failed_runs(runs: &[Run]) -> Result<Vec<FailedRun>, Error> {
    let mut failed = Vec::new();

    for run in runs {
        if !run.is_finished() {
            continue;
        }

        let Some(exit_status) = run.exit_status() else {
            continue;
        };

        if exit_status.success() {
            continue;
        }

        failed.push(FailedRun {
            id:     run.id(),
            reason: failure_reason(run, exit_status)?,
        });
    }

    Ok(failed)
}
```

Use `try_fold` only when fallible accumulation stays compact:

```rust
pub fn total_size(files: &[FileEntry]) -> Result<u64, Error> {
    files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.size()?)
            .ok_or(Error::SizeOverflow)
    })
}
```

## Exceptions

- Use a loop for a simple transform when Clippy or the project lint set prefers it.
- Use an iterator chain for branching logic only when each step is named clearly and Clippy accepts it.
- Use `for_each` for fluent APIs where side effects are intentionally local and Clippy does not object.
