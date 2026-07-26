# Contributing to stack

## Before your first PR

This project requires a signed [Contributor License Agreement](CLA.md). A
bot comments on your first PR with instructions — you sign it once, by
commenting on the PR itself, and it's remembered for future contributions.

## Building and testing

```shell
cargo build
cargo test
```

No other setup is required beyond a working Rust toolchain. There's no
build step for `docs/` (plain HTML/CSS/JS, no bundler) or `examples/` (each
is a real, independently runnable project — see its own README).

## Project structure

```plaintext
src/
  main.rs              entry point only: parses CLI args, dispatches
  core/                the actual logic — OS-agnostic
    manifest.rs           stack.toml parsing (read-only, via the toml crate)
    manifest_edit.rs       stack.toml mutation for `add`/`remove` (via toml_edit,
                           preserves comments/formatting/key order)
    orchestrate.rs         where every subcommand's real behavior lives
    toolchain.rs           vfox/uv resolution for [language.*]
    caddy.rs               Caddy admin-API routing
    process.rs             spawn/track/kill child processes
    state.rs, registry.rs, projects.rs   the three on-disk JSON stores under ~/.stack/
    shell.rs               shell hook script generation (pwsh/cmd)
    placeholder.rs          {VAR_NAME} resolution in [run]/[service.*] commands
    pinned.rs               pinned vfox/uv/caddy versions
  platform/            the one place OS-specific behavior is allowed
    windows.rs             the only platform actually exercised so far
    unix/                  designed to the standard POSIX pattern, not yet
                           verified on real Mac/Linux hardware
  interface/
    cli.rs                clap subcommand definitions, dispatches into core::orchestrate
```

`core` never touches the OS directly for anything behavior-specific — it
calls through `platform`, which has exactly one implementation compiled in
per target via `#[cfg]`.

## Code style

No narrative comments — no "verified live," no referencing how or why
something was debugged, no pointing at documents that don't ship in this
repo. A comment is only worth adding when it documents a genuinely
non-obvious invariant that a reasonable refactor could silently break —
keep it to one line stating the fact, not the story of how it was found.

## Pull requests

GitHub Actions CI may not run on your PR immediately — run `cargo build`
and `cargo test` locally before opening it regardless. Keep PRs scoped to
one change; explain the *why* in the PR description, not in code comments.
