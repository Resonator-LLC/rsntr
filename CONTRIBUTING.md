# Contributing

Thank you for helping. Bug reports, fixes, tests and documentation are all
welcome.

## Before you start

- For anything larger than a small fix, open an issue first so we can
  agree on the approach.
- Read [AGENTS.md](AGENTS.md). It is written for coding agents but is the
  shortest accurate guide to the repository for humans too: layout, build
  and test commands, and the rules the code keeps.

## Making a change

1. Fork, branch from `main`.
2. Keep the change focused. Add or update tests with it.
3. Run what CI runs:

   ```sh
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   (cd client && npm ci && npm run typecheck && npm test && npm run build)
   ```

4. Open a pull request that says what changed and why.

## Sign your commits (DCO)

We use the [Developer Certificate of Origin](https://developercertificate.org/)
instead of a contributor license agreement. Add a sign-off line to every
commit to certify that you wrote the change or have the right to submit it
under the project's license:

```sh
git commit -s
```

which adds `Signed-off-by: Your Name <you@example.com>`.

Contributions are accepted under the Apache License 2.0 (see `LICENSE`).

## Trademarks

Contributing code does not grant rights to the Resonator name or logo;
see [TRADEMARKS.md](TRADEMARKS.md).

## Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
