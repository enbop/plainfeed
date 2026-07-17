# Wasmtime deployment

Plainfeed runs the reader and synchronizer as separate WASIp2 components over
one preopened data checkout. Build both components with the required Tokio WASI
configuration:

```sh
scripts/bootstrap-git-dependencies.sh
RUSTFLAGS="--cfg tokio_unstable" cargo build --release \
  -p plainfeed-sync --bin plainfeed-sync --target wasm32-wasip2
cargo build --release -p plainfeed-server --target wasm32-wasip2
```

Provide the generic HTTPS remote URL and credentials through inherited guest
environment variables. `PLAINFEED_GIT_USERNAME` and
`PLAINFEED_GIT_PASSWORD` work for generic Basic authentication. A GitHub PAT
may instead use `PLAINFEED_GITHUB_TOKEN`; it is never written to Git config or
`.plainfeed/sync.toml`.

Run a forced pull before starting the reader:

```sh
wasmtime run \
  --env PLAINFEED_REMOTE_URL \
  --env PLAINFEED_GIT_USERNAME \
  --env PLAINFEED_GIT_PASSWORD \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed-sync.wasm force

wasmtime serve \
  -S cli=y \
  --addr 127.0.0.1:8080 \
  --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed_server.wasm
```

Have the host scheduler invoke the same sync component with `tick` every 30
seconds. A tick publishes dirty reader state after 30 idle seconds or five
minutes of continuous mutations. Without due state, it performs no network
request until the last successful pull is at least five minutes old. `force`
immediately performs the applicable pull or state publication cycle, while
`status` never uses the network and does not require credentials:

```sh
wasmtime run \
  --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed-sync.wasm status
```

The host must preopen only the data checkout needed by the guest. Keep token
files outside that checkout and unset credential environment variables after
manual runs.

## Conflict recovery

Plainfeed pauses synchronization instead of merging when ownership, validation,
or fast-forward rules fail. The reader continues serving the last valid feed
and shows a warning. Inspect the machine-readable details with `status` or read
`.plainfeed/conflict.toml` directly.

Repair the cause without changing ownership boundaries: restore local
`content/**` and `config/**` to the canonical commit, repair invalid producer
content on the remote, or manually reconcile an unexpected remote `state/**`
change. Explicitly acknowledge the inspected report, then force a new cycle:

```sh
wasmtime run --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed-sync.wasm acknowledge-conflict
# Run `force` with the network and credential options shown above.
```

Acknowledgement removes only the local report; it does not change Git or data
files. The next forced cycle validates the repair and recreates a report if the
problem remains. Until acknowledgement, `tick` and `force` remain blocked.

See [operations.md](operations.md) for pending-push recovery, interrupted local
activation recovery, bounded failure behavior, native Git diagnostics, and the
complete regression suite.
