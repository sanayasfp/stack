# react-frontend

A minimal Vite + React app — the case where `stack up` isn't part of the
workflow at all.

## What this demonstrates

Frontend dev servers you're already watching in a terminal don't need
`stack` to spawn or track them. Once `stack setup` has wired the shell hook
once, `cd`-ing into this folder already puts the pinned Node version first
on `PATH` — so running `npm run dev` by hand (the same way you already would
without `stack`) picks up the right Node version for free. There's no
`[run]` in this project's `stack.toml` and no route through Caddy — Vite
serves on its own default port directly.

## Run it

```
cd examples/react-frontend
npm install
npm run dev
```

No `stack up` step. Compare with `examples/node-api`, where `[run]` *is*
set because that process needs a stable routed domain and needs to keep
running without a terminal watching it.
