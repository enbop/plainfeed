# Plainfeed

Plainfeed is a personal, file-first information stream and reading server. It
serves Markdown entries, records reader state in TOML, and runs as a long-lived
WASIp2 service under Wasmtime. It has no database and is not coupled to GitHub.

The first usable slice supports:

- A newest-first timeline of titles and summaries with explicit source links.
- Fast channel switching for broad topics and project-specific streams.
- Automatic read markers after an entry remains visible.
- Favorites and plain-text personal comments.
- Atomic state-file replacement.
- A responsive, progressively enhanced interface.

## Data format

The versioned protocol is documented in [`spec/v1.md`](spec/v1.md). In short:

```text
data/
├── config/channels.toml
├── content/2026/07/example-entry.md
└── state/entries/example-entry.toml
```

Content is producer-owned Markdown with TOML front matter. `channels` route an
entry into curated navigation streams while `tags` remain open-ended metadata.
Mutable reader state is kept separately. `examples/data` is a complete fixture
that can be copied as the starting point for a data repository.

## Run with Wasmtime

Requirements used by the initial implementation:

- Rust with the `wasm32-wasip2` target.
- Wasmtime 46 or a compatible runtime with WASIp2 socket support.

Build and run the combined Axum reader and synchronization service:

```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build --release \
  -p plainfeed-service --target wasm32-wasip2
wasmtime run \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed-service.wasm \
  127.0.0.1:8080 /data
```

Open <http://127.0.0.1:8080/>. The guest owns its Tokio TCP listener and runs
the synchronization loop in the same single-threaded runtime. It forces a pull
at startup, evaluates local work every 30 seconds, pulls remote content at most
every five minutes, and batches reader state for publication after 30 idle
seconds or five minutes of continuous changes. No host timer is required.

Set `PLAINFEED_REMOTE_URL` and the generic Git credentials or
`PLAINFEED_GITHUB_TOKEN` described in [deployment.md](docs/deployment.md) for a
synchronized data checkout. Without a configured remote the same service runs
as a local-only reader.

Use a copy of `examples/data` if you do not want reader actions to modify the
tracked fixture.

## Verify

```bash
cargo fmt --all -- --check
cargo test --workspace
scripts/smoke-wasmtime.sh
scripts/smoke-wasmtime-service.sh
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
crates/plainfeed-server/  # HTTP-neutral reader rendering and proxy fallback
crates/plainfeed-service/ # Axum listener and autonomous synchronization
spec/                     # Versioned public file protocol
web/                      # Static progressive-enhancement assets
examples/data/            # Example content and state repository
experiments/              # Independent Git/WASI research
```
