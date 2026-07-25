# stack

**Native, zero-container multi-project dev environment manager. No Docker, no VMs.**

stack runs your project's languages, databases, and dev server as plain
child processes and routes a real local domain to whichever one you're
working on — no containers, no idle hypervisor, nothing running you didn't
ask for.

**Windows — available now** (PowerShell & cmd) · macOS — coming soon · Linux — coming soon

[Documentation](https://stackenv.dev) · [Examples](examples) · [Changelog](https://stackenv.dev/changelog.html)

## Quick look

```toml
# stack.toml
[project]
name = "acme-api"

[language]
php = "8.3.1"

[service.mysql]
version = "8.0.35"

[run]
command = "php -S 127.0.0.1:{port} -t public"
```

```shell
$ stack up
Loaded C:\Users\you\acme-api\stack.toml
  project: acme-api
  domain: acme-api.localhost
  languages: ["php"]
  services: ["mysql"]
  php: C:\Users\you\.vfox\cache\php\v-8.3.1\...\php.exe -> PHP 8.3.1 (cli)
  service.mysql: started (pid 41232, port 3306)
  run: php -S 127.0.0.1:52140 -t public  (pid 41244, port 52140)
  routed: http://acme-api.localhost -> 127.0.0.1:52140
```

Every project pinning the same language+version shares one binary from a
central store — nothing copied per project. Every project pinning the same
service+version shares one running instance, isolated by schema.

## Install

```shell
winget install stack.cli
```

or `scoop`, `choco`, `cargo install stack` — see [Getting Started](https://stackenv.dev/getting-started.html) for all four.

Then, once:

```shell
stack setup
```

Wires the shell hook and installs the tools stack delegates to (vfox, uv,
Caddy) at pinned, tested versions.

## Why not Docker

Containers solve "works on every machine" by shipping an entire OS layer
per project. stack solves the same problem differently: pin exact versions
per project through the version managers you'd install anyway (vfox, uv),
share one binary across every project at that version, and run everything
as plain processes — no virtualization overhead, no idle daemon between
projects.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
