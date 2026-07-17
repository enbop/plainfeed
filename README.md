# Plainfeed

Plainfeed is a personal, file-first information stream and reading server. It
serves Markdown entries, records reader state in TOML, and runs as a WASI HTTP
component under Wasmtime. It has no database and no runtime dependency on Git
or GitHub.

The first usable slice supports:

- A newest-first reading timeline.
- Markdown bodies with raw HTML and unsafe link schemes neutralized.
- Automatic read markers after an entry remains visible.
- Favorites and plain-text personal comments.
- Atomic state-file replacement.
- A responsive, progressively enhanced interface.

## Data format

The versioned protocol is documented in [`spec/v1.md`](spec/v1.md). In short:

```text
data/
├── content/2026/07/example-entry.md
└── state/entries/example-entry.toml
```

Content is producer-owned Markdown with TOML front matter. Mutable reader state
is kept separately. `examples/data` is a complete fixture that can be copied as
the starting point for a data repository.

## Run with Wasmtime

Requirements used by the initial implementation:

- Rust with the `wasm32-wasip2` target.
- Wasmtime 43 or a compatible runtime with WASI HTTP 0.2 support.

Build and serve the component:

```bash
cargo build --release -p plainfeed-server --target wasm32-wasip2
wasmtime serve \
  -S cli=y \
  --addr 127.0.0.1:8080 \
  --dir examples/data::/data \
  target/wasm32-wasip2/release/plainfeed_server.wasm
```

Open <http://127.0.0.1:8080/>. The guest always reads `/data`; the `--dir`
mapping selects the host directory and grants the component file access. The
`-S cli=y` capability is required for filesystem, clock, and standard I/O
imports. No network capability is granted to the component itself.

Use a copy of `examples/data` if you do not want reader actions to modify the
tracked fixture.

## Verify

```bash
cargo fmt --all -- --check
cargo test --workspace
scripts/smoke-wasmtime.sh
```

The smoke test builds the WASIp2 component, serves a temporary copy of the
fixture, exercises the page plus read/favorite/comment mutations over HTTP, and
checks the resulting TOML state.

## Frontend choice

The first UI uses vendored htmx 2.0.10 and a small plain JavaScript read
observer. htmx fits a server-rendered personal reader because it adds partial
HTML form updates without a Node build chain, client-side router, or JSON state
store. The page still renders useful content before JavaScript runs. See
[`docs/frontend.md`](docs/frontend.md) for the decision and upgrade boundary.

## Project layout

```text
crates/plainfeed-core/    # Format, indexing, and state transitions
crates/plainfeed-server/  # HTML rendering and WASI HTTP adapter
spec/                     # Versioned public file protocol
web/                      # Static progressive-enhancement assets
examples/data/            # Example content and state repository
experiments/              # Independent Git/WASI research
```
