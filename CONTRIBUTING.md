# Contributing

## Licensing of contributions

No CLA. The project is dual licensed MIT OR Apache-2.0, and contributions
are taken under those same terms: unless you state otherwise, anything you
submit for inclusion is dual licensed as above, per Apache-2.0 section 5.

Sign off your commits (`git commit -s`) to certify the Developer Certificate
of Origin (https://developercertificate.org) - that you wrote the patch or
otherwise have the right to submit it under this license.

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
