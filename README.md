# Plainfeed

[![CI](https://github.com/enbop/plainfeed/actions/workflows/ci.yml/badge.svg)](https://github.com/enbop/plainfeed/actions/workflows/ci.yml)

> Plainfeed is an early-stage personal project. Its data format is versioned,
> but deployment and synchronization behavior may still change before 1.0.

Plainfeed is a personal, file-first information stream and reading server. It
serves Markdown entries, records reader state in TOML, and runs as a long-lived
WASIp2 service under Wasmtime. It has no database and is not coupled to GitHub.

The first usable slice supports:

- A newest-first title-and-summary timeline with dedicated Markdown reading views.
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

For AI research tasks and scheduled content producers, copy
[`PLAINFEED-CONTENT-GUIDE.md`](PLAINFEED-CONTENT-GUIDE.md) into the root of the
data repository and instruct the producer to read it before each run. It is a
self-contained writing, deduplication, path-ownership, and validation contract.

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
  127.0.0.1:18437 /data
```

Open <http://127.0.0.1:18437/>. The guest owns its Tokio TCP listener and runs
the synchronization loop in the same single-threaded runtime. On first access,
the settings page asks for the HTTPS Git remote and GitHub PAT and persists them
under `/data/.plainfeed/`. The host path may be a completely empty directory:
after settings are saved, Plainfeed initializes it as a Git checkout of remote
`main`. It refuses to bootstrap over unrelated existing files. Saving wakes the
internal synchronization task immediately. It otherwise evaluates local work
every 30 seconds, pulls remote content at most every five minutes, and batches
reader state for publication after 30 idle seconds or five minutes of
continuous changes. No host timer is required. Environment overrides are
described in [deployment.md](docs/deployment.md).

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

## Releases

Version tags publish `plainfeed-service.wasm`, a sibling-file
`plainfeed.fungi.md`, and `SHA256SUMS` through GitHub Releases. Download all
three files into one directory, verify the checksums, and apply the service:

```bash
sha256sum --check SHA256SUMS
fungi service apply plainfeed ./plainfeed.fungi.md --start --yes
```

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

## License

Plainfeed is available under the [MIT License](LICENSE). Development is
AI-assisted and the corresponding commits retain explicit attribution.
