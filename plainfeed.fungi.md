---
fungi: service/v1
id: plainfeed

run:
  provider: wasmtime
  source:
    file: target/wasm32-wasip2/release/plainfeed-service.wasm
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

Runs the locally built Plainfeed WASIp2 component. Both the component and this
service file remain local; no GitHub Release asset is downloaded.

Fungi provides a service-specific persistent data directory and mounts it at
`/data` inside the component.
