# Single-command Wasmtime service experiment

## Question

Can one `wasm32-wasip2` command own both the HTTP listener and an autonomous
background task under `wasmtime run`, instead of relying on `wasmtime serve`
plus an external scheduler?

## Result

Yes. The experiment in `experiments/wasmtime-run-service` uses crates.io Tokio
1.52.4 with its WASIp2 networking implementation. One current-thread runtime:

- binds a `tokio::net::TcpListener` inside the guest;
- serves `/health` and `/experiment/status` over loopback HTTP; and
- runs a spawned 250 ms heartbeat task that atomically updates a file under the
  preopened data directory.

The verification script waits for two heartbeat updates before sending the
first HTTP request. This proves that the background task is driven by the
long-lived command itself rather than by incoming request handling.

Run the proof with:

```sh
scripts/verify-wasmtime-run-service.sh
```

The tested invocation is equivalent to:

```sh
wasmtime run \
  -S inherit-network=y \
  --dir /path/to/data::/data \
  target/wasm32-wasip2/debug/wasmtime-run-service-experiment.wasm \
  127.0.0.1:18090 /data
```

## Architectural consequence

Plainfeed can provide a single long-running WASI command that owns its Tokio
runtime, HTTP listener, and synchronization loop. This avoids treating a
`wasi:http/proxy` request handler as a singleton daemon and removes the normal
deployment requirement for a host timer or a second Wasmtime process.

The existing proxy reader and one-shot sync command remain useful as a stable
fallback while the combined service is developed.

## Remaining proof steps

Completed after the initial probe:

- Axum 0.8.9, Hyper 1.10.1, and Tokio 1.52.4 compile and run under Wasmtime
  46.0.1.
- The production service reuses the real reader routes without changing the
  file protocol.
- A non-`Send` local task runs the existing synchronization cycle; a Wasmtime
  fixture proves autonomous startup pull and state publication.

Remaining:

1. Add live, server-rendered sync status and control endpoints.
2. Define explicit graceful shutdown behavior when WASI signal support permits.
