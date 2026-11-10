# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

History before 0.2.3 lives in [docs/changelog.html](docs/changelog.html) and the
[GitHub releases](https://github.com/sanayasfp/stack/releases).

## [Unreleased]

## [0.2.3] - 2026-11-10

`stack describe` now works outside a project, and fresh PHP installs come with the extensions most frameworks expect.

### Added
- `stack describe -p/--profile <name>` — describe a profile instead of a project. Takes precedence inside a project; can't be combined with a project name.

### Fixed
- `stack describe` outside a project now describes the default profile instead of failing. It only errors when there's neither a `stack.toml` nor a default profile. (#1)
- Fresh PHP installs now also enable `openssl`, `curl`, `mbstring`, `zip`, `intl`, `gd`, `mysqli`, `exif` and `sqlite3`, each only when its DLL ships in `ext/`. An existing install's `php.ini` is left as is. (#3)

[Unreleased]: https://github.com/sanayasfp/stack/compare/v0.2.3...HEAD
[0.2.3]: https://github.com/sanayasfp/stack/compare/v0.2.2...v0.2.3
