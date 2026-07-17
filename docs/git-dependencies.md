# WASIp2 Git dependency maintenance

Plainfeed currently builds its Git adapter from two ignored source checkouts
under `refs/`. The exact upstream revisions and the compatibility patches are
tracked by `scripts/bootstrap-git-dependencies.sh`:

| Dependency | Pinned revision | Plainfeed patch |
| --- | --- | --- |
| Gitoxide | `402babdf82d6709c6a8c804e07138a8a004c54af` | `experiments/git-wasi/patches/gitoxide-wasip2.patch` |
| memmap2 | `7d76ad3157383db5670fd7e012f44de42aa7444b` | `experiments/git-wasi/patches/memmap2-wasip2.patch` |

Run the bootstrap script after a fresh source checkout. It clones only a
missing dependency, checks the exact commit, and refuses to alter an unexpected
or dirty checkout. Reqwest is an unmodified crates.io dependency.

This patch-based setup keeps the Plainfeed repository reproducible before
dedicated fork repositories exist. The preferred maintenance path is still one
thin fork per upstream project: keep a minimal upstreamable topic branch, pin a
separate Plainfeed integration branch, and replace the local path dependency
with an immutable fork commit. Plainfeed-specific transport and synchronization
policy stay in `plainfeed-git` and `plainfeed-sync-core`, not in either fork.

The Gitoxide patch enables WASI temporary-pack handling, substitutes buffered
reads while indexing a received pack, and avoids an unavailable process ID.
The memmap2 patch supplies read-only buffered mappings on WASI. Both buffer data
in guest memory, so `plainfeed-git` enforces explicit HTTP response and total
repository-size limits.
