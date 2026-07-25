# fastapi-dev

A real FastAPI app, scaffolded with `uv init` + `uv add fastapi "uvicorn[standard]"`
and verified end-to-end (`GET /` and `GET /health` both respond correctly
under `uv run uvicorn main:app`).

## What this demonstrates

This project's `stack.toml` sets `[run].external = true`. You run
`uvicorn --reload` yourself, in your own terminal, exactly how you already
would without `stack` — same command, same reload workflow. `stack`'s only
job is validating port 8000 and routing `fastapi-dev.localhost` to it; it
never spawns or supervises the process. See the docs site's
[Manifest Reference, "Why `[run].external` exists"](https://stackenv.dev/manifest.html#run-external).

## Run it

1. Install dependencies:
   ```
   uv sync
   ```
2. Activate the pinned Python version and register the route (run this in
   the project folder):
   ```
   stack up
   ```
   This prints that nothing's listening on port 8000 yet — expected, since
   you haven't started uvicorn.
3. In your own terminal, start the dev server yourself:
   ```
   uv run uvicorn main:app --reload --port 8000
   ```
4. Visit `http://fastapi-dev.localhost` — routed straight through to the
   server you just started.
