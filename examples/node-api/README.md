# node-api

A minimal Node HTTP server, no framework — `[run]` spawning a process whose
own reload story actually works cleanly under `stack`.

## What this demonstrates

`node --watch` (built into Node 18+) runs as a two-process tree — a
supervisor and a worker — and restarts the worker cleanly on file change.
Unlike `uvicorn --reload` (see `examples/fastapi-dev`), this doesn't need
`[run].external`: `stack` spawns and supervises it directly.

## Run it

```
cd examples/node-api
stack up
```

Visit `http://node-api.localhost`. Edit `server.js` — the change is picked
up on the next request with no manual restart.
