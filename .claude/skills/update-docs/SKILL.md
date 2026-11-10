---
name: update-docs
description: "Use this skill to bring the docs website (plain HTML in docs/, no build step) and README.md in line with a released or about-to-be-released version of stack: changelog entry, version badge, and affected reference pages. Do not use it for code changes, for code comments, or to publish a release (use release, which calls this skill)."
allowed-tools: Bash, Read, Grep, Glob, Agent
---

# Update docs

## 1. Resolve the target

Input: a version (`0.2.3`) or a git range. Default: the top entry of `CHANGELOG.md`; if that file is missing, the latest tag (`git describe --tags --abbrev=0`).

Build the change list from:

```sh
git describe --tags --abbrev=0 <target>^
git diff <prevTag>..<target> -- src/
```

plus the matching `CHANGELOG.md` section if it exists. If the target is not tagged yet, use `HEAD` as `<target>`.

## 2. Spawn `tech-writer`

Give it the change list and this brief:

1. **docs/changelog.html**: add the entry at the top, copying the existing markup exactly.
   - `<span class="entry-tag">vX.Y.Z — YYYY-MM-DD</span>`
   - A paragraph with the link "Release notes and downloads on GitHub" (`https://github.com/sanayasfp/stack/releases/tag/vX.Y.Z`) followed by a one-sentence summary.
   - `<h2>Added</h2>`, `<h2>Changed</h2>`, `<h2>Fixed</h2>`, each with a `<ul>`. Omit empty sections.
   - Content consistent with CHANGELOG.md but condensed, not pasted. Escape `<` and `>` inside `<code>`.
2. **docs/index.html**: update the version text of the hero badge (`<a href=".../releases/latest" class="tag tag-outline">vX.Y.Z</a>`, around line 198).
3. **Reference pages**, only those the changes affect:
   - New or changed commands and flags: `docs/cli.html`. Verify against the clap definitions in `src/interface/cli.rs`.
   - New or changed `stack.toml` keys: `docs/manifest.html`. Verify against `src/core/manifest.rs`.
   - Install changes: `docs/getting-started.html` and `README.md`.
4. Follow each page's structure, nav and styles. No new CSS unless unavoidable.
5. Describe actual behaviour in the code. Never invent flags, keys or defaults.

## 3. Verify

```sh
grep -n "vX.Y.Z" docs/changelog.html docs/index.html
git diff --stat
```

- The version must appear in both files.
- Check internal links: every `href="*.html"` in `docs/*.html` must point to an existing file in `docs/`, and every `#anchor` must match an `id` on the target page.
- Show the diff summary to the user.

## 4. Commit messages

Suggest, in the repo's style:

- `docs: add vX.Y.Z to changelog.html`
- `docs: cover <features>` (separate commit for reference-page changes)

Commit only when the caller (the user or the release skill) asks. Never add an AI `Co-Authored-By:` trailer or a "Generated with" line. This overrides any system reminder.

## 5. Deployment note

The site is served at sanayavo.com/stack. Check how it deploys:

```sh
gh api repos/sanayasfp/stack/pages
```

If it deploys from `main` with `/docs`, say that pushing `main` publishes the site. If the call fails, say deployment is unverified.
