# Git on WASI Experiments

This directory contains isolated build and runtime probes for pure-Rust Git
libraries. The experiments do not consider bindings to C implementations such
as libgit2.

## Candidates

- `gix-probe`: Gitoxide APIs with separately testable local and network feature
  sets.
- `gix-source-probe`: the same feature boundary against `refs/gitoxide`, used
  for disposable upstream-source patches. It requires that reference checkout
  and is not a standalone reproducible probe.

The probes target `wasm32-wasip2` and are intended to run with a repository
directory preopened in Wasmtime.

See [`../../docs/research/gix-wasi.md`](../../docs/research/gix-wasi.md) for the
dated results and architectural recommendation.

## Local read probe

```bash
cd experiments/git-wasi/gix-probe
cargo build --target wasm32-wasip2
wasmtime run \
  --dir ../../..::/repo \
  target/wasm32-wasip2/debug/plainfeed-gix-wasi-probe.wasm \
  inspect /repo
```

## Patched source write probe

This requires `refs/gitoxide`, `refs/memmap2`, and the disposable WASI
compatibility changes documented in the research note. Reqwest is taken
unmodified from crates.io; the probe injects its own WASIp2-compatible Hickory
resolver. The remaining experimental diffs are stored in `patches`.

```bash
git clone https://github.com/GitoxideLabs/gitoxide refs/gitoxide
git clone --branch v0.9.11 https://github.com/RazrFalcon/memmap2-rs refs/memmap2
git -C refs/gitoxide apply ../../experiments/git-wasi/patches/gitoxide-wasip2.patch
git -C refs/memmap2 apply ../../experiments/git-wasi/patches/memmap2-wasip2.patch
cd experiments/git-wasi/gix-source-probe
RUSTFLAGS="--cfg tokio_unstable" \
  cargo build --target wasm32-wasip2 \
  --no-default-features --features local,smart-http-push
wasmtime run \
  --dir ../../../refs::/refs \
  target/wasm32-wasip2/debug/plainfeed-gix-source-wasi-probe.wasm \
  init-commit /refs/new-repository
```

## Single-threaded HTTPS probe

```bash
wasmtime run \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  experiments/git-wasi/gix-source-probe/target/wasm32-wasip2/debug/plainfeed-gix-source-wasi-probe.wasm \
  https-get \
  'https://github.com/GitoxideLabs/gitoxide.git/info/refs?service=git-upload-pack'
```

## Async gix fetch probe

This transport buffers each smart-HTTP request and response in memory. It is a
correctness probe for small personal repositories, not a streaming production
transport.

```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build \
  --manifest-path experiments/git-wasi/gix-source-probe/Cargo.toml \
  --target wasm32-wasip2 --no-default-features \
  --features local,https-async-reqwest-rustls

wasmtime run \
  -S inherit-network=y \
  -S allow-ip-name-lookup=y \
  --dir /tmp \
  experiments/git-wasi/gix-source-probe/target/wasm32-wasip2/debug/plainfeed-gix-source-wasi-probe.wasm \
  async-fetch https://github.com/octocat/Hello-World.git /tmp/plainfeed-gix-fetch
```

## Constrained smart-HTTP push probe

This probe is not a general push implementation. It supports one existing
SHA-1 branch and exactly one new fast-forward commit. It sends a complete
non-delta pack and requires a successful receive-pack status report.

Build it with:

```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build \
  --manifest-path experiments/git-wasi/gix-source-probe/Cargo.toml \
  --target wasm32-wasip2 --no-default-features \
  --features local,smart-http-push
```

For a reproducible loopback test, prepare a bare remote and client clone, then
run the fixture server:

```bash
git clone --bare refs/plainfeed-playground /tmp/plainfeed-push-remote.git
git clone /tmp/plainfeed-push-remote.git /tmp/plainfeed-push-client
python3 experiments/git-wasi/support/git-smart-http-server.py \
  /tmp/plainfeed-push-remote.git --port 18080
```

In another shell, create exactly one commit and push it from Wasmtime:

```bash
wasmtime run --dir /tmp \
  experiments/git-wasi/gix-source-probe/target/wasm32-wasip2/debug/plainfeed-gix-source-wasi-probe.wasm \
  create-push-commit /tmp/plainfeed-push-client \
  refs/heads/main refs/heads/main wasi-push.txt 'written inside Wasmtime'

wasmtime run -S inherit-network=y --dir /tmp \
  experiments/git-wasi/gix-source-probe/target/wasm32-wasip2/debug/plainfeed-gix-source-wasi-probe.wasm \
  async-push http://127.0.0.1:18080/repo.git \
  /tmp/plainfeed-push-client refs/heads/main refs/heads/main
```

For GitHub, the transport accepts a token only through the guest environment
and refuses to send it over non-HTTPS URLs. Do not put the token in a command
argument, file, or Git configuration:

```bash
read -s PLAINFEED_GITHUB_TOKEN
export PLAINFEED_GITHUB_TOKEN
wasmtime run \
  --env PLAINFEED_GITHUB_TOKEN \
  -S inherit-network=y -S allow-ip-name-lookup=y --dir /tmp \
  experiments/git-wasi/gix-source-probe/target/wasm32-wasip2/debug/plainfeed-gix-source-wasi-probe.wasm \
  async-push https://github.com/OWNER/REPOSITORY.git \
  /tmp/plainfeed-push-client refs/heads/main refs/heads/main
unset PLAINFEED_GITHUB_TOKEN
```

Authenticated GitHub push was verified against
`spore-bot/plainfeed-playground`. Commit
`2aa41c7a9058e40c0dc922bc8b950f949760eb17` was created and pushed entirely by
the Wasmtime guest, then independently verified through an SSH fetch and
native `git fsck --full`.
