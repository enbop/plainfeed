# Gix on WASI Evaluation

Date: 2026-07-16

## Decision

Use `gix` as a promising local repository engine behind a narrow Plainfeed
adapter, but do not rely on it as the complete synchronization implementation.

For the first architecture:

- The WASI application may inspect repositories and create local Git history
  through `gix` after the identified WASI fixes are available.
- The WASI application may fetch through a small custom async smart-HTTP
  transport. The current proof buffers requests and responses and is suitable
  only for small repositories.
- A host-side or GitHub API synchronization adapter should remain available
  while the constrained guest-side push experiment is hardened.
- The file store remains usable without either adapter.

`gix` does not currently expose a complete push porcelain because upstream
push support is incomplete. Plainfeed nevertheless assembled a deliberately
narrow push from public lower-level APIs and completed it under Wasmtime
against a local smart-HTTP server. Its built-in blocking Reqwest adapter
requires OS threads, but Plainfeed's custom implementation of gix's public
async transport trait now performs both fetch and this constrained push.
Networking is therefore an adapter problem rather than a Tokio or WASI
impossibility.

## Environment

```text
rustc 1.97.0 (2d8144b78 2026-07-07)
cargo 1.97.0 (c980f4866 2026-06-30)
wasmtime 43.0.0 (be23469ec 2026-03-20)
target: wasm32-wasip2
gix: 0.85.0
gitoxide source: 402babd (2026-07-15)
gitoxide compatibility fork: enbop/gitoxide 7b4c806
reqwest: crates.io 0.13.4, unmodified
memmap2 source: 7d76ad3 (v0.9.11)
memmap2 compatibility fork: enbop/memmap2-rs 7163e10
tokio: 1.52.3 with --cfg tokio_unstable
rustls: 0.23.42
rustls-rustcrypto: 0.0.2-alpha
```

The initial experiment used ignored `refs/gitoxide` and `refs/memmap2` source
checkouts. The production build now consumes exact commits from the public
`enbop` compatibility forks over HTTPS. `refs/reqwest` remains only as
historical reference material and is not used by the build. The writable remote
fixture is available through the ignored `refs/plainfeed-data-fixture` path.

## Capability Results

| Capability | Result | Notes |
| --- | --- | --- |
| Compile local repository APIs | Pass | `gix` with `status`, `sha1`, and `tree-editor` compiles to `wasm32-wasip2`. |
| Open a preopened repository | Pass | Ran under Wasmtime against the Plainfeed source repository. |
| Calculate worktree status | Pass | The Wasmtime probe reported the expected untracked changes. |
| Initialize a repository | Conditional pass | Works after the tempfile PID compatibility patch described below. |
| Write blobs and trees | Conditional pass | A blob and root tree were written inside Wasmtime. |
| Create a commit and update `HEAD` | Conditional pass | Native Git read the result and `git fsck --full` passed. |
| Stage arbitrary worktree changes | Partial | The index plumbing exists, but upstream still lists the add-and-commit porcelain workflow as incomplete. Plainfeed can directly edit trees for files it owns. |
| Compile fetch protocol plumbing | Conditional pass | Works after allowing `gix-tempfile` on WASI. |
| Tokio TCP networking | Pass | Tokio 1.51 added WASIp2 networking; 1.52.3 compiles with `tokio_unstable`. |
| Pure-Rust HTTPS request | Conditional pass | Unmodified crates.io Reqwest, an injected Hickory DNS resolver, Rustls, and `rustls-rustcrypto` reached GitHub from Wasmtime and received a valid smart-HTTP advertisement. |
| HTTPS fetch through current gix adapter | Fail | The gix blocking Reqwest adapter always spawns a worker OS thread, which WASIp2 does not provide. |
| HTTPS fetch through custom async gix transport | Conditional pass | Public GitHub protocol-v2 fetch, pack/index writing, object lookup, and remote-ref updates completed under Wasmtime. The probe buffers whole HTTP messages. |
| Repeated no-change fetch | Conditional pass | A second fetch reused the repository, received no pack, and reported all three ref updates as `NoChangeNeeded`. |
| Private GitHub HTTPS authentication | Conditional pass | A repository-scoped fine-grained PAT was inherited by the guest as an environment variable and authenticated both receive-pack advertisement and push. |
| SSH/file transport in the guest | Not viable | Existing transports invoke external processes, which WASI does not provide. |
| Push porcelain | Fail | Gitoxide's upstream status documents general push and send-pack/receive-pack client plumbing as incomplete. |
| Constrained smart-HTTP push | Conditional pass | A Plainfeed probe pushed one SHA-1 fast-forward commit with a complete, non-delta pack under Wasmtime to both a local receive-pack fixture and GitHub, then parsed `report-status`. |
| Host-side SSH synchronization | Pass | Native Git pushed the Wasmtime-generated commit to a private test repository. |

## Verified Local Write

The patched source probe performed these operations inside Wasmtime:

1. Initialized a repository in a preopened directory.
2. Wrote the contents of `probe.txt` as a blob.
3. Built and wrote a tree with `Repository::edit_tree()`.
4. Created the first commit with `Repository::commit_as()`.
5. Updated `HEAD` through the gix ref transaction.

The resulting commit was:

```text
a053378778382055381965fae2989313e3e76283
```

Native Git verified the object graph with `git fsck --full`, read the commit and
file, and pushed it to the playground repository's `main` branch.

## Upstream Compatibility Gaps

The public compatibility-fork branches contain the minimal changes first
proved in disposable source checkouts. Plainfeed pins their immutable commit
IDs; the original patch files remain as historical review artifacts.

### 1. Permit temporary files on WASI

`gix-pack` enables temporary files for streaming pack input, but its dependency
is currently declared only when `target_arch != "wasm32"`. This groups WASI
with browser WebAssembly even though WASI provides a capability-based
filesystem.

The experiment changed the condition to include `target_os = "wasi"`. With
that change, the blocking fetch protocol layer compiled successfully.

### 2. Avoid process IDs on WASI

`gix-tempfile` records `std::process::id()` so forked processes do not clean up
one another's files. Rust's WASI implementation panics because process IDs do
not exist on that platform.

The experiment uses a fixed ID for WASI. A WASI component cannot fork, so this
preserves the relevant single-process cleanup behavior. After this change,
repository initialization and ref transactions ran successfully.

### 3. Select a portable TLS crypto provider

Reqwest's `rustls` feature selects AWS-LC, which contains C code and did not
build with the available WASI toolchain. The custom transport depends directly
on unmodified Reqwest with its `rustls-no-provider` feature and installs
`rustls-rustcrypto 0.0.2-alpha` at application startup. No gix-transport or
Reqwest source patch is needed for this selection.

This proves a pure-Rust TLS path is technically viable, but the provider is
explicitly experimental and is not yet a production recommendation.

### 4. Configure asynchronous DNS on WASI

The default Reqwest resolver and Tokio's hostname lookup use
`spawn_blocking()`. Instead of enabling Reqwest's built-in `hickory-dns`
feature, the experiment implements Reqwest's public `Resolve` trait, constructs
a Hickory resolver directly with an explicit DNS configuration, and injects it
through `ClientBuilder::dns_resolver()`. This compiles and resolves GitHub
successfully without OS threads or a Reqwest fork.

The unmodified-Reqwest path completed an HTTPS advertisement request, a public
fetch with pack/index persistence, private PAT authentication, and a real
GitHub push under Wasmtime. An independent SSH fetch observed the resulting
commit, and native `git fsck --full` passed.

### 5. Replace read-only memory maps with buffered reads

Gitoxide uses `memmap2` for pack/index reads and while resolving a newly
received pack. `memmap2` selects its unsupported stub backend on WASI, so the
first full fetch received a valid pack but failed while writing its index.

The experiment makes two deliberately memory-oriented substitutions:

- Gitoxide reads the temporary pack into a `Vec<u8>` while generating its
  index on WASI.
- The memmap2 stub implements read-only mappings by reading the requested file
  range into a `Vec<u8>`; mutable and anonymous mappings remain unsupported.

This behavior is acceptable for Plainfeed-sized repositories but needs memory
limits before production use. Native Git verified the resulting pack and index.

These changes need upstream discussion and tests before Plainfeed depends on a
fork or patched release.

## HTTPS Boundary

Tokio 1.51 added `wasm32-wasip2` networking support through upstream PR #7933.
The referenced Heap-Hop PR #1 is a later example and CI suite for that support,
not the patch that originally added it. It confirms the same operating boundary
we observed: a current-thread runtime works, TCP and UDP require
`tokio_unstable` and inherited network capability, and Tokio's built-in DNS is
not available. In the tested Tokio 1.52.3 release, networking still requires:

```text
RUSTFLAGS="--cfg tokio_unstable"
```

With a current-thread Tokio runtime, async Hickory DNS, Reqwest's no-provider
Rustls feature, and the RustCrypto provider, the Wasmtime guest requested:

```text
https://github.com/GitoxideLabs/gitoxide.git/info/refs?service=git-upload-pack
```

The response was:

```text
status=200 OK
content_type=application/x-git-upload-pack-advertisement
body_bytes=611569
```

The private playground endpoint returned `401` without credentials and
authenticated successfully once a repository-scoped fine-grained PAT was
inherited by the guest. This verifies DNS, TCP, TLS, and HTTP Basic credential
transport without storing the token in Git configuration or command arguments.

The gix blocking Reqwest adapter still cannot run because it creates a worker
with `std::thread::spawn()` to support duplex Git request/response streaming.
The experiment now supplies a custom implementation of gix's public async
transport trait. It buffers each POST body before issuing the Reqwest request,
then buffers the response before gix consumes it. This avoids OS threads and
proves the fetch path, at the cost of memory proportional to the pack response.

Under Wasmtime it fetched `octocat/Hello-World` using protocol v2, wrote a pack
containing 13 objects, updated three `refs/remotes/origin/*` refs, and completed
a second no-change fetch. Native `git fsck --full`, `git show-ref`, and
`git cat-file` all succeeded against the resulting repository.

Wasmtime issue #12102, now closed, concerned upgrading the separate
host-provided `wasi:tls` interface from its client-only Phase 1 to Phase 2.
Phase 2 adds server-side TLS, connection metadata, and a simpler asynchronous
flow. It is a possible future way to move TLS and certificate handling into the
host, but Reqwest/Rustls does not use that interface in this probe; TLS and
certificate verification run inside the guest.

For Plainfeed these are distinct transport choices:

| Route | Current evidence | Main trade-off |
| --- | --- | --- |
| Guest TLS: Tokio + Reqwest + Rustls | End-to-end HTTPS proven in this experiment | Needs DNS and crypto-provider compatibility work; keeps transport portable across compatible WASI runtimes. |
| Host TLS: `wasi:tls` | Wasmtime's Phase 2 tracking issue is closed, but not integrated or tested here | Can centralize TLS policy in the host; couples the component to the proposed host interface and runtime support. |

The host-TLS route does not remove Gitoxide's missing push support or its
blocking adapter's use of OS threads. Those are separate Git transport-layer
limitations.

## Constrained Smart-HTTP Push

The `smart-http-push` probe implements the smallest useful receive-pack client
without modifying gix itself. It intentionally supports only:

- SHA-1 repositories.
- Existing `refs/heads/*` branches.
- Exactly one new commit whose sole parent is the advertised remote tip.
- A complete pack containing the new commit and its entire tree, without
  deltas or object-negotiation optimization.
- `report-status` or `report-status-v2` success and rejection parsing.
- Optional GitHub PAT authentication from `PLAINFEED_GITHUB_TOKEN`, sent as an
  HTTPS Basic password and never printed or persisted.

The probe rejects non-HTTPS URLs when the token environment variable is set
and disables HTTP redirects so credentials cannot be redirected to another
authority. It does not support new branches, multiple commits, merges, tags,
deletes, force pushes, atomic updates, push options, signed pushes, SHA-256,
LFS, sideband progress, or delta compression.

The end-to-end fixture used a loopback Python HTTP adapter backed by native
`git receive-pack --stateless-rpc`. The client commit and network request both
ran inside a `wasm32-wasip2` guest under Wasmtime. The observed result was:

```text
old=a053378778382055381965fae2989313e3e76283
new=66d4913eaae6a43d73b703b1dfdc17e9dc0d8146
pack_bytes=411
remote_status=unpack ok
remote_status=ok refs/heads/main
```

Native Git then ran `git fsck --full`, read the two-commit history, and read
the pushed file from the bare remote. A second Wasmtime push advanced the
remote independently, after which the first client refused its stale update
before uploading a pack because its new commit no longer had the advertised
remote tip as its parent.

The same Wasmtime binary then pushed to a private GitHub test repository over HTTPS
with a repository-scoped fine-grained PAT inherited through
`PLAINFEED_GITHUB_TOKEN`. The first authenticated handshake exposed a protocol
compatibility detail: GitHub honors `Git-Protocol: version=1` by prepending a
standalone `version 1` packet, while gix-transport 0.57.2's async handshake tries
to parse that packet as the first ref and reports `MissingDelimitingNullByte`.
The custom transport now omits the protocol-version header for receive-pack,
which requests the standard V0-style advertisement used by ordinary push
clients; fetch continues to request protocol v2.

The verified GitHub result was:

```text
old=a053378778382055381965fae2989313e3e76283
new=2aa41c7a9058e40c0dc922bc8b950f949760eb17
pack_bytes=450
remote_status=unpack ok
remote_status=ok refs/heads/main
```

An independent SSH fetch observed `origin/main` at the new commit, read
`wasi-github-smart-http-push.txt`, and passed `git fsck --full`. The token value
was never printed, placed in an argument, written to Git configuration, or
stored in the repository.

## Recommended Plainfeed Boundary

```text
Plainfeed file store
        |
        +-- WASI service
        |     +-- parse/index/query files
        |     +-- update reader state
        |     +-- optional gix local commits
        |
        +-- optional guest smart-HTTP adapter
        |     +-- async Git smart HTTP
        |     +-- constrained single-commit push
        |     +-- bounded buffering for personal repositories
        |
        +-- fallback synchronization adapters
              +-- reconcile before publishing updates
              +-- native Git or GitHub Git Database API
```

The adapter must expose explicit synchronization states and must never make the
file format depend on Git or GitHub. If remote synchronization is unavailable,
the reader and local file history should continue to work.

## Follow-up Work

- Turn the remaining Gitoxide source edits into focused upstream issues or
  pull requests.
- Replace whole-response buffering with bounded storage or streaming, or set a
  strict repository-size limit.
- Decide whether an experimental RustCrypto provider is acceptable or whether
  TLS should use a more mature provider or Wasmtime's host TLS interface.
- Extend the push probe to multiple linear commits by walking back to the
  advertised remote tip, then add server-rejection fixtures.
- Define the exact ownership boundary between the guest and host for commits.
- Add a host synchronization protocol with locking and conflict reporting.
- Test fetch/reconciliation with concurrent agent changes in the test data repository.
- Decide whether the constrained push should become a maintained Plainfeed
  adapter, an upstream contribution, or remain a research probe.

## Upstream References

- [Tokio PR #7933: add `wasm32-wasip2` network support](https://github.com/tokio-rs/tokio/pull/7933)
- [Heap-Hop Tokio PR #1: WASIp2 runnable examples and support matrix](https://github.com/Heap-Hop/tokio/pull/1)
- [Wasmtime issue #12102: upgrade `wasmtime-wasi-tls` to Phase 2](https://github.com/bytecodealliance/wasmtime/issues/12102)
- [WASI TLS Phase 2 proposal discussion](https://github.com/WebAssembly/wasi-tls/issues/13)
