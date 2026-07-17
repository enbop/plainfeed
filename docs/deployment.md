# Wasmtime deployment

Plainfeed normally runs one long-lived WASIp2 command. Axum owns the HTTP
listener while an autonomous task runs the same synchronization policy over the
preopened data directory:

```sh
RUSTFLAGS="--cfg tokio_unstable" cargo build --release \
  -p plainfeed-service --target wasm32-wasip2
```

Run the service with only the data directory preopened:

```sh
wasmtime run \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed-service.wasm \
  127.0.0.1:8080 /data
```

The host data path may be an empty directory or an existing valid Plainfeed Git
checkout. On first access Plainfeed redirects to `/settings`. Enter the HTTPS
Git remote and GitHub PAT there; the form stores them in
`.plainfeed/service-settings.toml` and requests synchronization immediately.
For an empty directory, Plainfeed fetches remote `main`, validates the complete
snapshot, creates the local branch and index, and materializes the worktree.
Only `.plainfeed/` and an unborn `.git/` may already exist during bootstrap;
other files cause initialization to stop rather than overwrite local data.
The token is stored as local plain text but is never rendered back into HTML,
written to Git configuration, or committed by Plainfeed. Restrict access to the
host data directory and the service itself.

Environment variables remain an operational override. Generic Basic
authentication uses `PLAINFEED_GIT_USERNAME` and `PLAINFEED_GIT_PASSWORD`; a
GitHub PAT may use `PLAINFEED_GITHUB_TOKEN`. `PLAINFEED_REMOTE_URL` overrides
the saved remote. Pass only the variables in use with Wasmtime's `--env` flag.

The service performs an initial configuration and repository check before
entering its normal 30-second scheduling loop; no host timer or second process
is required. Saving settings wakes that same task with a forced cycle.

The service publishes dirty reader state after 30 idle seconds or five minutes
of continuous mutations. Without due state, it performs no Git network request
until the last successful pull is at least five minutes old.

The one-shot sync command remains available for offline status and manual
recovery:

```sh
wasmtime run \
  --dir /path/to/plainfeed-data::/data \
  target/wasm32-wasip2/release/plainfeed-sync.wasm status
```

The host must preopen only the data checkout needed by the guest. Unset
credential environment variables after manual runs.

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
