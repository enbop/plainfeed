---
fungi: service/v1
id: plainfeed

run:
  provider: wasmtime
  source:
    file: plainfeed-service.wasm
  args:
    - 127.0.0.1:18437
    - /data
  mounts:
    - from: $fungi.service.data
      to: /data

publish:
  http:
    tcp:
      port: 18437
    client:
      kind: web
      path: /
---

# Plainfeed

Place this file beside `plainfeed-service.wasm`. Fungi provides a
service-specific persistent data directory and mounts it at `/data` inside the
component.
