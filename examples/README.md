# Examples

Four small, real projects — each shows a different corner of `stack.toml`
rather than the same happy path four times.

| Example | Shows |
| --- | --- |
| [`fastapi-dev/`](fastapi-dev) | `[run].external` — run your dev server yourself, in your own terminal, and let stack just route to it. |
| [`node-api/`](node-api) | `[run]` spawning and supervising a process directly — `node --watch`'s reload story actually works cleanly under `stack`. |
| [`react-frontend/`](react-frontend) | No `[run]` at all — a frontend dev server you run by hand, where `stack` only exists to put the right Node version on `PATH`. |
| [`laravel-app/`](laravel-app) | `[language]` + `[service]` + `[tool]` together — PHP, a shared MySQL instance, and Composer's shadow function, in the shape a real framework project actually uses. |

Each folder has its own `README.md` with exact run instructions. None of
these need Docker, and none of them need anything installed beyond what
`stack setup` already gets you.
