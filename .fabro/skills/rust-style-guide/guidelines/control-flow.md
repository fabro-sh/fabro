# Control Flow

## Rule

Use clarity-first branching: prefer `?`, `let else`, `if let`, and `match` to make branches and exits explicit, and keep mutation in small, validated scopes.

## Why

Control flow carries invariants, error paths, and state transitions. Explicit branches and small mutable scopes are easier for agents to modify safely than clever expression chains, hidden exits, or partially updated state.

## Do

- Use `?` when the local code only needs to propagate a fallible result.
- Use early returns for invalid inputs, missing prerequisites, and permission checks.
- Use `let else` when a required pattern must be present and the fallback exits the current scope.
- Use `if let` when only one pattern needs special handling.
- Use `while let` for loops that repeatedly consume optional or result-like values.
- Use `match` when multiple variants matter, exhaustiveness matters, or each branch has distinct behavior.
- Keep `match` arms small; extract a helper when a branch grows past the local decision.
- Prefer naming meaningful enum variants over `_` when future variants should force a revisit.
- Use match guards only when the guard is short and directly tied to the arm.
- Keep the main path linear after validation and setup.
- Use `let mut` for local accumulators, builders, counters, and staged values; keep mutable scopes small and return to immutable locals once setup is complete.
- Validate fallible inputs before mutating long-lived state; prefer computing a new value locally and assigning it once when that avoids partial updates.
- Use `std::mem::take` or `std::mem::replace` when moving a field out while leaving the struct valid.
- Treat Clippy as authoritative for local control-flow idioms; refactor instead of adding local bypasses ([rustc and Clippy lints](rustc-and-clippy-lints.md)).

## Avoid

- Do not write combinator chains that hide branching or side effects; [Option and Result idioms](option-and-result-idioms.md) owns the combinator-vs-branching line.
- Do not use `match` on `bool`; use `if` with a named condition.
- Do not use `_` to ignore meaningful domain states.
- Do not deeply nest `if` or `match` blocks when guard clauses would make exits clearer.
- Do not use `let else` when the fallback contains substantial recovery logic; use `match`.
- Do not replace explicit error handling with `unwrap` or `expect`.
- Do not force a functional style when a small mutable local is clearer.
- Do not mutate object state before fallible validation unless the partial state is intentional and documented.

## Example

Prefer visible exits and exhaustive domain handling:

```rust
pub fn plan_action(request: Request) -> Result<Action, Error> {
    let Some(user_id) = request.user_id() else {
        return Err(Error::MissingUserId);
    };

    let command = Command::parse(request.command())?;

    if !request.permissions().can_run(&user_id, &command) {
        return Err(Error::Forbidden { user_id });
    }

    let action = match command {
        Command::Start { target } => {
            let target = Target::try_new(target)?;
            Action::Start { target }
        }
        Command::Stop { target } => Action::Stop { target },
        Command::Status => Action::Status,
    };

    Ok(action)
}
```

Validate first, then mutate the owned state in a small block:

```rust
pub struct UserAccount {
    email:  EmailAddress,
    labels: Vec<String>,
    active: bool,
}

impl UserAccount {
    pub fn update(&mut self, update: UserUpdate) -> Result<(), Error> {
        let email = match update.email() {
            Some(value) => Some(EmailAddress::try_new(value)?),
            None => None,
        };

        let mut labels = Vec::new();
        for label in update.labels() {
            labels.push(Label::try_new(label)?.into_string());
        }

        if let Some(email) = email {
            self.email = email;
        }

        self.labels = labels;

        if update.deactivate() {
            self.active = false;
        }

        Ok(())
    }
}
```

Use combinators for simple local transformations:

```rust
impl User {
    pub fn display_name(&self) -> String {
        self.nickname()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.username())
            .to_owned()
    }
}
```

## Exceptions

- Use combinators when the transformation is short, linear, and side-effect free.
- Use `_` for intentionally ignored variants in tests, logging, metrics, or external `#[non_exhaustive]` enums.
- Use a `match` even for two cases when it documents a domain state machine or prepares for likely new variants.
- Mutate as you go when each step is independently valid and there is no meaningful rollback requirement.
