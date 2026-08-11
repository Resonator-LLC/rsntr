# Contributing

## CLA

All external contributions require a signed Contributor License Agreement.
The project is dual licensed (AGPL-3.0-only plus commercial licenses), which
depends on a single copyright holder; the CLA assigns the necessary rights.
Contact the maintainer before opening a merge request.

## Style

- ASCII only in all files: no em dashes, no smart quotes. Use non-ASCII
  Unicode only when necessary for other languages.
- Rust edition 2024; `cargo fmt` before committing.
- Code comments only for non-obvious constraints.
- Terminology: "mod"/"modulation" everywhere; the word "dialect" must not
  appear in code, docs, or comments.

## Tests

- Test after writing; never leave code untested.
- Tests are colocated with each crate; run `cargo test -p <crate>` for the
  crate you changed and `cargo test --workspace` before declaring done.
- Nothing is done without a passing test run.
