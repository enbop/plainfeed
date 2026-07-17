# WASIp2 Git dependency maintenance

Plainfeed consumes two public compatibility forks over HTTPS, pinned to exact
commit IDs in Cargo manifests:

| Dependency | Public fork branch | Pinned revision | Upstream base |
| --- | --- | --- | --- |
| Gitoxide | [`spore-bot/gitoxide:plainfeed/wasip2`](https://github.com/spore-bot/gitoxide/tree/plainfeed/wasip2) | `7b4c806ed6175f21cc4d813ffcb197df95452197` | `402babdf82d6709c6a8c804e07138a8a004c54af` |
| memmap2 | [`spore-bot/memmap2:plainfeed/wasip2`](https://github.com/spore-bot/memmap2/tree/plainfeed/wasip2) | `7163e105159cb9e95d952e42d390e36ccd83e5c1` | `7d76ad3157383db5670fd7e012f44de42aa7444b` |

Reqwest is an unmodified crates.io dependency. A normal Plainfeed build no
longer needs ignored source checkouts or a patch-application bootstrap step.
Cargo fetches the public forks and verifies the revisions through `Cargo.lock`.

The optional `scripts/bootstrap-git-dependencies.sh` command clones the exact
fork revisions under ignored `refs/` directories for source inspection,
experiments, and fork maintenance. It never applies patches and refuses dirty
or unexpected revisions. The patch files under
`experiments/git-wasi/patches/` remain historical, reviewable snapshots of the
original changes.

The Gitoxide fork enables WASI temporary-pack handling, substitutes buffered
reads while indexing a received pack, and avoids an unavailable process ID.
The memmap2 fork supplies read-only buffered mappings on WASI. Both buffer data
in guest memory, so `plainfeed-git` enforces explicit HTTP response, object,
pack, and total repository-size limits.

## Updating a fork

Do not make changes on the fork's default branch and do not open an upstream PR
as part of routine Plainfeed dependency maintenance. Work on
`plainfeed/wasip2`, keep upstream as a read-only remote, and push explicitly to
the `spore-bot` fork. After testing, update every Cargo `rev`, regenerate
`Cargo.lock`, and rerun the full native and Wasmtime regression suites.

Upstream contributions are a separate, explicit task. If authorized, split the
compatibility commit into minimal upstreamable changes rather than sending the
Plainfeed integration branch wholesale. Plainfeed-specific transport,
constrained push, and synchronization policy stay in `plainfeed-git` and
`plainfeed-sync-core`, not in either fork.
