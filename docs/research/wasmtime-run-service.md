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

1. Replace the hand-written HTTP probe with a small supported HTTP server stack.
2. Keep serving health/status requests during a delayed outbound HTTPS fetch.
3. Integrate the real reader routes without changing the file protocol.
4. Run the existing synchronization cycle as a background task with a
   single-writer guard and explicit shutdown behavior.
5. Add live, server-rendered sync status and control endpoints.
