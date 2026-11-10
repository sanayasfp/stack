---
name: test-changes
description: "Use this skill to add or extend Rust unit tests for changed behaviour in the stack repo (uncommitted changes, current branch vs main, or a given range), then run cargo test and report real counts. Do not use it to review code (use review-changes), change production behaviour, or update docs."
allowed-tools: Bash, Read, Grep, Glob, Agent
---

# Test changes

## 1. Identify changed behaviour

Default scope: uncommitted changes plus the current branch against `main`. An argument can be a range or a worktree path.

```sh
git diff HEAD
git diff main...HEAD
```

List each changed behaviour (function, command, manifest key) and the existing tests that cover it. Tests live in-module in `#[cfg(test)] mod tests`. There is no `tests/` directory.

## 2. Spawn `test-writer`

Give it the behaviour list and these constraints:

- Add tests in the same file, inside `#[cfg(test)] mod tests`.
- Run with `cargo test`. Target one test with `cargo test <name>`.
- No network. No real registry, no real PATH, no real `~/.stack`. Use temp dirs and pure functions.
- Guard platform-specific tests with `#[cfg(windows)]` / `#[cfg(unix)]`.
- Tests must fail without the change.
- Same code style as production: no narrative comments.
- Do not change production code. If a change is needed to make code testable, report what and why instead of doing it silently.

## 3. Run the suite yourself

```sh
cargo test
```

Report the real totals: passed, failed, ignored. Do not reuse the numbers the test-writer reported. Include the output of any failure.

Also run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`. Report their result separately. Both are blocking in this repo.

## 4. Report

- Tests added, with file and name.
- Behaviour still untested, and why.
- Production-code changes the test-writer made or proposed.
