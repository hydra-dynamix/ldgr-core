# Contributing to ldgr

Thanks for your interest in contributing.

## Development setup

A recent stable Rust toolchain is all you need. SQLite is bundled through
`rusqlite`, so there are no system dependencies.

```sh
git clone https://github.com/hydra-dynamix/ldgr-core
cd ldgr-core
cargo test
```

## Running checks

Before opening a pull request, make sure all three pass:

```sh
cargo fmt --check
cargo clippy --all-targets    # the codebase is clippy-clean; keep it that way
cargo test                    # unit tests + end-to-end smoke tests
```

The smoke suite in `tests/cli_smoke.rs` spawns the real binary in temp
directories and asserts exact CLI output. If you change user-facing output,
update the matching assertions in the same change.

## Code guidelines

- Use clear, descriptive names; avoid abbreviations and single-letter
  variables outside tight mathematical contexts.
- No stub or placeholder implementations — finish the feature or track the
  remainder in an issue.
- Multi-statement ledger writes must go through `in_write_transaction` in
  `src/store/helpers.rs` so partial failures roll back.
- Schema changes go in `src/store/schema.rs`. The production release starts at
  schema version 1; future schema changes must add forward migrations instead of
  rewriting the released shape.
- Keep business logic in `src/store/`; `src/cli/` handlers should parse
  arguments, call the store, and render.

## Commits and pull requests

- Use conventional commits: `feat:`, `fix:`, `refactor:`, `docs:`, `chore:`,
  `test:`.
- Keep changes focused; unrelated refactors belong in separate PRs.
- Describe what changed and how you validated it.

## Licensing

By contributing, you agree that your contributions will be licensed under the
[Apache License, Version 2.0](LICENSE-APACHE), as described in the README.
