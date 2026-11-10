---
name: review-changes
description: "Use this skill to review code changes in the stack repo before merge or release: uncommitted changes, the current branch vs main, a git range, or another worktree. It runs the fmt, clippy and test gates, then delegates to the reviewer agent (and security-reviewer when the diff is security-sensitive). Do not use it to fix code, write tests (use test-changes), update docs (use update-docs) or cut a release (use release)."
allowed-tools: Bash, Read, Grep, Glob, Agent
---

# Review changes

Review only. Fix nothing unless the user asks.

## 1. Pick the scope

The argument can be a git range (`v0.2.2..HEAD`), a branch, or a worktree path.

Default scope: uncommitted changes plus the current branch against `main`.

```sh
git status --porcelain
git diff HEAD
git diff main...HEAD
git log --oneline main..HEAD
```

On a clean `main` with no argument, ask the user what to review. Do not guess.

## 2. Run the gates

Run each from the repo root (or the target worktree). All three are blocking.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Report every failure with its output. Do not stop at the first failure.

## 3. Spawn `reviewer`

Pass the diff scope and these project rules (from CONTRIBUTING.md):

- No narrative comments. Only a one-line comment for a non-obvious invariant. No "verified live", no debugging stories.
- `src/core/` never touches the OS directly. OS-specific code lives only in `src/platform/` (`windows.rs`, `unix/`), selected with `#[cfg]`.
- `stack.toml` edits go through `toml_edit` in `src/core/manifest_edit.rs` and must keep comments, formatting and key order. `manifest.rs` is read-only.
- `src/interface/cli.rs` holds clap definitions and only dispatches to `core::orchestrate`. No logic there.
- The crate is Windows-first. Unix code must still compile.
- User-visible changes need matching docs in `docs/` and `README.md`. Flag the gap, do not write it.

## 4. Spawn `security-reviewer` in parallel (conditional)

Only when the diff touches one of these:

- Downloads or extraction: `ureq`, `zip` (zip-slip), `src/core/pinned.rs` versions and checksums.
- Process spawning or command building: `process.rs`, `placeholder.rs`, `shell-words`.
- Shell hook generation: `shell.rs` (injection into pwsh/cmd).
- PATH or registry persistence: `winreg`, `src/platform/`.
- Script trust-on-first-use, `.env` loading, the Caddy admin API.

Otherwise skip it and say so.

## 5. Report

Merge all findings into three groups, each with `file:line`:

- **Blocker**: failing gate, correctness bug, security issue, rule violation that must change.
- **Should-fix**: likely problem or missing docs/tests.
- **Nit**: style, naming, small cleanups.

Start with the gate results (pass/fail per command). If nothing is found, say so plainly.
