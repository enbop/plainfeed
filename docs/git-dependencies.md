# WASIp2 Git dependency maintenance

Plainfeed consumes `git-wasip2` as its only Git implementation dependency.
Each layer pins the next layer to an exact public commit:

| Layer | Repository or branch | Pinned revision | Pinned by |
| --- | --- | --- | --- |
| Git/WASIp2 API | [`enbop/git-wasip2`](https://github.com/enbop/git-wasip2) | `97ce8071124ba296a5eba827ccfc63836c58f33e` | Plainfeed |
| Gitoxide compatibility | [`enbop/gitoxide:plainfeed/wasip2`](https://github.com/enbop/gitoxide/tree/plainfeed/wasip2) | `283374581e2c8fa2fea28079e1f284a1b0c7fbfc` | git-wasip2 |
| memmap2 compatibility | [`enbop/memmap2-rs:plainfeed/wasip2`](https://github.com/enbop/memmap2-rs/tree/plainfeed/wasip2) | `7163e105159cb9e95d952e42d390e36ccd83e5c1` | Gitoxide compatibility branch |

Reqwest is an unmodified crates.io dependency. Cargo follows this transitive
chain and verifies every revision through `Cargo.lock`. Plainfeed does not use
a root Cargo patch and does not declare Gitoxide or memmap2 directly.

The optional `scripts/bootstrap-git-dependencies.sh` command clones all three
exact revisions under ignored `refs/` directories for source inspection,
experiments, and fork maintenance. It never applies patches and refuses dirty
or unexpected revisions. The patch files under `experiments/git-wasi/patches/`
remain historical, reviewable snapshots of the original changes.

`git-wasip2` owns the generic bounded Smart HTTP fetch/push and local repository
primitives. `plainfeed-git` contains only Plainfeed repository policy. The
Gitoxide fork enables WASI temporary-pack handling, substitutes buffered reads
while indexing a received pack, and avoids an unavailable process ID. The
memmap2 fork supplies read-only buffered mappings on WASI. Both buffer data in
guest memory, so `git-wasip2` enforces explicit HTTP response, object, pack,
and total repository-size limits.

## Updating a fork

Do not make changes on a fork's default branch and do not open an upstream PR
as part of routine Plainfeed dependency maintenance. Work on
`plainfeed/wasip2`, keep upstream as a read-only remote, and push explicitly to
the `enbop` fork. After testing, update the immediate consumer's Cargo `rev`,
regenerate its `Cargo.lock`, and rerun its full native and Wasmtime regression
suites before advancing the next layer.

Upstream contributions are a separate, explicit task. If authorized, split the
compatibility commit into minimal upstreamable changes rather than sending the
integration branch wholesale. Generic WASIp2 transport and constrained push
stay in `git-wasip2`; Plainfeed synchronization policy stays in
`plainfeed-git` and `plainfeed-sync-core`.
