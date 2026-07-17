# Wasmtime deployment

Plainfeed normally runs one long-lived WASIp2 command. Axum owns the HTTP
listener while an autonomous task runs the same synchronization policy over the
preopened data checkout:

```sh
scripts/bootstrap-git-dependencies.sh
RUSTFLAGS="--cfg tokio_unstable" cargo build --release \
  -p plainfeed-service --target wasm32-wasip2
```

Provide the generic HTTPS remote URL and credentials through inherited guest
environment variables. `PLAINFEED_GIT_USERNAME` and
`PLAINFEED_GIT_PASSWORD` work for generic Basic authentication. A GitHub PAT
may instead use `PLAINFEED_GITHUB_TOKEN`; it is never written to Git config or
`.plainfeed/sync.toml`.

Run the service. It performs the initial forced pull before entering its normal
30-second scheduling loop; no host timer or second process is required:

```sh
wasmtime run \
  --env PLAINFEED_REMOTE_URL \
  --env PLAINFEED_GIT_USERNAME \
  --env PLAINFEED_GIT_PASSWORD \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed-service.wasm \
  127.0.0.1:8080 /data
```

The service publishes dirty reader state after 30 idle seconds or five minutes
of continuous mutations. Without due state, it performs no Git network request
until the last successful pull is at least five minutes old.

The one-shot sync command remains available for offline status, manual recovery,
and compatibility deployments:

```sh
wasmtime run \
  --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed-sync.wasm status
```

The host must preopen only the data checkout needed by the guest. Keep token
files outside that checkout and unset credential environment variables after
manual runs.

## Compatibility mode

The earlier `wasi:http/proxy` reader and host-scheduled sync command remain
supported while the combined service matures:

```sh
RUSTFLAGS="--cfg tokio_unstable" cargo build --release \
  -p plainfeed-sync --bin plainfeed-sync --target wasm32-wasip2
cargo build --release -p plainfeed-server --target wasm32-wasip2
```

Run `plainfeed-sync.wasm force`, start `plainfeed_server.wasm` with
`wasmtime serve`, and invoke `plainfeed-sync.wasm tick` every 30 seconds as
documented by the older topology. Both modes share the same files, Git history,
conflict policy, and recovery commands.

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
