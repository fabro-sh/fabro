Production runtime code must not panic on any path reachable from CLI input,
  HTTP requests, workflow definitions, external services, storage, subprocesses,
  or normal environment failure.

  Use Result for recoverable or reportable failures, preserving the source chain
  until the boundary. CLI boundaries render errors with miette. HTTP boundaries log
  the full internal chain and return a curated public API error.

  Panics are allowed only for:
  - tests, fixtures, and test-only helpers;
  - build scripts or dev tooling where failure happens before runtime;
  - hard-coded literals or generated constants whose validity is controlled by the
    source tree, preferably with `expect` explaining the invariant;
  - truly impossible internal invariants where continuing would be more dangerous
    than terminating.

  `unwrap()` is not allowed in production runtime code. `expect()` is allowed only
  when the message explains why the failure is impossible, not merely what failed.
  `panic!`, `todo!`, `unimplemented!`, and `unreachable!` require an explicit,
  reviewable justification.

  The practical review test should be:

  > Could this failure be caused by input, config, environment, I/O, network, time, concurrency, persisted state, or a third-party system?

  If yes, it is not a panic. Return an error.