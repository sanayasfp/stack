# laravel-app

**Honest label first:** this is a minimal, hand-written skeleton — `composer.json`,
a placeholder `public/index.php`, a `.env.example` — not a real
`composer create-project laravel/laravel` output. Composer isn't installed
in the environment this example was built in, so a real scaffold couldn't be
generated and verified here. What's real and accurate is the `stack.toml`
wiring: php + mysql + Composer, the actual shape a real Laravel project uses.

## Turn this into a real Laravel app

```
cd examples/laravel-app
composer create-project laravel/laravel .
```

This overwrites the placeholder files with the real framework. Keep
`stack.toml` as-is, except swap `[run].command` for:

```toml
command = "php artisan serve --host=127.0.0.1 --port={port}"
```

## What the manifest demonstrates

- **`[language] php`** — pinned per project via vfox.
- **`[service.mysql]`** — started once, shared with any other project
  pinning the same version, isolated by schema.
- **`[tool.composer]`** — stack fetches `composer.phar` itself at the pinned
  version. Once activated (`stack setup` + `cd` into this folder), running
  `composer install` bare uses this project's pinned PHP automatically —
  the composer-shadow feature, not something you have to invoke by hand.
- **`[run]`** — a plain PHP built-in server pointed at `public/`, routed to
  `laravel-app.localhost`.

## Run it (with the placeholder, before scaffolding)

```
stack up
```

Visit `http://laravel-app.localhost` — served by the placeholder
`public/index.php` until you run the real `composer create-project` step
above.
