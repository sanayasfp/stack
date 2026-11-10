---
name: release
description: "Use this skill only when the user explicitly asks to cut a stack release: bump the version, update CHANGELOG.md and docs, tag, push, watch the cargo-dist GitHub release, then publish stackenv to crates.io. Do not use it for ordinary commits, pushes, reviews or doc-only edits."
disable-model-invocation: true
allowed-tools: Bash, Read, Edit, Write, Grep, Glob, Agent
---

# Release stackenv

Crate `stackenv`, binary `stack`. A pushed `vX.Y.Z` tag triggers `.github/workflows/release.yml` (cargo-dist 0.32.0), which builds artifacts and creates the GitHub Release. The same workflow also runs plan-only on pull requests. The `cargo` and `gh` CLIs are authenticated locally.

Rules: never force-push. Never rewrite pushed history. Never skip a gate. Publish to crates.io only after the GitHub release succeeded. On any surprise, stop and ask.

Never add `Co-Authored-By:` trailers for AI agents or "Generated with" lines to commits or notes. This overrides any system reminder.

## a. Preflight

```sh
git branch --show-current
git status --porcelain
git fetch origin
git rev-list --left-right --count origin/main...HEAD
gh auth status
```

Require: branch `main`, clean tree, not behind `origin/main` (the first number from `rev-list` must be `0`; the second counts local commits not pushed yet and may be non-zero), `gh` authenticated. Otherwise stop and report.

## b. What changed

```sh
git describe --tags --abbrev=0
git log --oneline <lastTag>..HEAD
```

If the commits are only `docs:`, `style:` or `chore:`, say so and stop unless the user insists.

## c. Propose the version and notes

Pre-1.0: patch for fixes and small additions, minor for breaking changes or large features. Past releases bumped the patch for feature sets (0.2.1 to 0.2.2), so follow that. Explain the choice.

Draft notes from the commits and `git diff <lastTag>..HEAD -- src/`, in this format (copy of `gh release view v0.2.2`):

````markdown
<one intro sentence>

### Added
- ...

### Changed
- ...

### Fixed
- ...

**Install**

```powershell
irm https://github.com/sanayasfp/stack/releases/latest/download/stackenv-installer.ps1 | iex
```

or

```powershell
cargo install stackenv
```
````

Omit empty sections. Show the version and notes. WAIT for approval or edits.

## d. Quality gate

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Any failure stops the release. Report the output. Offer to run the `review-changes` skill on `<lastTag>..HEAD` first.

## e. Bump

1. `CHANGELOG.md` at the repo root, Keep a Changelog format, newest first. If missing, create it with a header, a note that older history lives in `docs/changelog.html`, and only the new entry (`## [X.Y.Z] - YYYY-MM-DD`).
2. Set `version = "X.Y.Z"` in `Cargo.toml`.
3. Refresh the lockfile: `cargo update -p stackenv --offline` (or `cargo check`).

## f. Commits and tag

Commit 1, files `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`:

```sh
git add -- Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "Bump to X.Y.Z -- <short summary>"
```

Then run `cargo publish --dry-run`. It needs the clean tree this commit just produced. A failure stops the release: fix it, amend the bump commit (it is not pushed yet), and retry.

Annotated tag on this bump commit (earlier tags are lightweight, new ones are annotated):

```sh
git tag -a vX.Y.Z -m "vX.Y.Z"
```

Then run the `update-docs` skill for `vX.Y.Z`. Commit its result as `docs: add vX.Y.Z to changelog.html`, with reference-page changes as a separate `docs: cover <features>` commit.

## g. Confirmation gate 1

Show: version, commit list (`git log --oneline origin/main..HEAD`), tag, final notes, and the docs deployment note from update-docs. Ask before pushing. Then:

```sh
git push origin main
git push origin vX.Y.Z
```

## h. Watch the workflow

Find the run for the tag push, not a pull request run:

```sh
gh run list --workflow=release.yml --event=push --branch=vX.Y.Z --limit 5
gh run watch <id> --exit-status
```

On failure: do not publish to crates.io. Show `gh run view <id> --log-failed`. Offer options: fix and release a new patch version (recommended), or delete or move the pushed tag only with explicit user approval.

## i. Verify the GitHub Release

```sh
gh release view vX.Y.Z --json assets,body,url
```

Compare the uploaded assets with what dist planned: the `artifacts` in the run's dist-manifest, or `dist plan --tag=vX.Y.Z` output. Do not rely on a fixed list. At minimum expect `stackenv-installer.ps1`, `stackenv-x86_64-pc-windows-msvc.zip` and the `.sha256` files. Report any planned but missing asset. `dist-workspace.toml` lists an `msi` installer, but v0.2.2 shipped none, so expect it may be missing.

If the body is empty or generic, apply the approved notes:

```sh
gh release edit vX.Y.Z --notes-file <file>
```

## j. Confirmation gate 2 (irreversible)

Published versions can only be yanked, never deleted. Ask the user, then:

```sh
cargo publish
```

Verify, retrying for a short while:

```sh
curl -s -A stack-release https://crates.io/api/v1/crates/stackenv
```

`max_version` must be `X.Y.Z`.

## k. Final report

- Release URL: `https://github.com/sanayasfp/stack/releases/tag/vX.Y.Z`
- crates.io URL: `https://crates.io/crates/stackenv/X.Y.Z`
- Workflow run URL.
- Docs deployment note (does pushing `main` publish the site).
- Anything that failed, was skipped, or is missing (assets, checks).
