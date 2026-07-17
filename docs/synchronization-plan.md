# Synchronization implementation plan

## Objective

Synchronize a single Plainfeed data repository with a generic HTTPS Git remote
from WASI while preserving a deliberately narrow conflict model:

- `main` is the only canonical branch.
- External producers own `content/**`.
- The repository owner owns `config/**`.
- Plainfeed owns `state/**`.
- `.plainfeed/**` is local synchronization metadata and is never committed.
- The synchronizer never performs a general merge or rebase.
- Ownership violations, history divergence, and genuine same-path conflicts
  stop synchronization for manual resolution.

The first deployment target is `spore-bot/plainfeed-playground` over HTTPS with
a repository-scoped token inherited by the WASI guest. GitHub is the initial
test remote, not a storage-format dependency.

External writers must follow the [producer contract](producer-contract.md).

## Implementation status

- Phase 0: the canonical data layout is published at playground commit
  `ff2845b` and passes the reader's Wasmtime smoke test.
- Phase 1: the provider-independent ownership audit, synchronization metadata,
  conflict report, dirty journal, and post-rename state markers are implemented
  with unit tests.
- Phase 2: the production `plainfeed-git` crate performs bounded authenticated
  HTTPS fetches under Wasmtime, inspects the remote tip and state tree, and
  passes repeated-fetch, invalid-credential, untouched-worktree, and native
  `git fsck --full` checks against the real playground repository.
- Phase 3 is the next implementation step.

## Runtime topology

Use two WASI components sharing one preopened data checkout:

```text
plainfeed-reader.wasm       wasmtime serve; reads content/config, writes state
plainfeed-sync.wasm         wasmtime run; fetches and publishes Git changes
           │
           └── /data        checkout of plainfeed-playground
```

`wasi:http/proxy` has no autonomous startup or timer callback, so a host timer
invokes the sync command. Keeping synchronization out of reader requests also
prevents Git or network failures from delaying normal reading.

## Repository contract

The data repository has this shape:

```text
config/
content/
state/
.gitignore
.git/

# ignored local runtime data
.plainfeed/
  sync.toml
  update.lock/
  dirty/
  staging/
  conflict.toml
```

Rules enforced before activating or publishing a change:

1. External remote changes may modify `content/**`.
2. `config/**` changes are accepted from the remote but are validated as
   owner-managed configuration.
3. A remote change to `state/**` is accepted only when it exactly matches the
   last state tree successfully published by this Plainfeed instance.
4. Plainfeed-generated commits may modify only `state/**`.
5. `.plainfeed/**` must be ignored and must never occur in a commit.
6. `main` must advance by fast-forward. Force pushes and divergent history are
   manual conflicts.
7. Content and configuration must fully validate before becoming visible.

## Local synchronization state

Store human-readable, uncommitted status in `.plainfeed/sync.toml`:

```toml
format = "plainfeed.sync/v1"
remote = "origin"
branch = "refs/heads/main"
last_remote_oid = "..."
last_state_tree_oid = "..."
last_pull_at = "2026-07-17T00:00:00Z"
last_push_at = "2026-07-17T00:00:00Z"
last_error = ""
```

Every successful reader-state mutation creates a unique marker under
`.plainfeed/dirty/`. A sync cycle snapshots the marker names at its start and
deletes only that snapshot after a successful push. Mutations that occur during
the sync leave newer markers behind, preventing a lost-dirty race.

## Phased implementation

### Phase 0: Establish the live data checkout

1. Turn `plainfeed-playground` into the live data repository.
2. Remove or archive the probe-only files currently at its repository root.
3. Move the current example content, channel configuration, and desired reader
   state into its canonical directories.
4. Add `.plainfeed/` to the data repository's `.gitignore`.
5. Confirm the initial remote `state/**` tree as the trusted state baseline.
6. Stop mounting `plainfeed/examples/data` as writable live data.
7. Mount the playground checkout as `/data` and rerun the reader smoke test.

Exit criteria:

- Reader actions change only the playground checkout.
- The source repository stays clean while the reader is used.
- Native Git can validate and push the initial data repository.

### Phase 1: Define synchronization boundaries

1. Add a `plainfeed-sync-core` crate independent of HTTP and GitHub.
2. Model remote tips, ownership audits, dirty snapshots, sync status, and
   conflict reports.
3. Add `.plainfeed/conflict.toml` using a versioned, human-readable format.
4. Make state writes create dirty markers only after the atomic state rename
   succeeds.
5. Document the producer contract for external AI writers.

Exit criteria:

- Unit tests prove that content/state path ownership is enforced.
- A mutation racing with dirty-marker cleanup cannot be lost.
- No Git networking is introduced yet.

### Phase 2: Integrate authenticated pull-only Git

1. Create a narrow `plainfeed-git` adapter around the proven gix/WASI transport.
2. Pin the required gitoxide and memmap2 compatibility revisions; keep Reqwest
   unmodified with the injected WASIp2 DNS resolver.
3. Verify authenticated upload-pack/fetch against the private playground
   repository; the earlier experiment proved authenticated receive-pack/push
   but did not complete this production pull path.
4. Fetch `origin/main`, persist objects, and inspect the remote tree without
   changing the live files.
5. Enforce repository-size and response-buffer limits.

Exit criteria:

- Repeated private HTTPS fetches work under Wasmtime with and without changes.
- Tokens appear only in inherited guest environment and never in logs or Git
  configuration.
- A failed fetch leaves the live data untouched.

### Phase 3: Activate remote content safely

1. Export the fetched `content/**` and `config/**` trees into
   `.plainfeed/staging/<remote-oid>/`.
2. Parse and validate the complete staged snapshot.
3. Acquire `.plainfeed/update.lock/` with an atomic directory creation.
4. Replace live content/config from the staged snapshot while preserving
   `state/**`.
5. Remove files deleted by the remote and retain orphaned state as specified by
   the v1 format.
6. Regenerate the worktree index and advance the local canonical ref only after
   activation succeeds, so native Git sees the same base tree.
7. Roll back or leave the old snapshot active on any failure.

The reader checks the update lock and returns a short retryable response rather
than reading a half-activated snapshot.

Exit criteria:

- New, changed, and deleted remote entries appear after sync.
- Invalid remote content produces a conflict report and does not become live.
- Reader state is unchanged by content activation.

### Phase 4: Add the scheduled WASI sync command

1. Build `plainfeed-sync.wasm` as a WASIp2 command component.
2. Implement `tick`, `force`, and `status` commands.
3. Run a forced pull before starting the reader.
4. Let a host scheduler run `tick` every 30 seconds.
5. Perform network pull only when the last successful pull is at least five
   minutes old, unless `force` is requested.

Exit criteria:

- Restart, no-change, offline, and interrupted-sync cases are repeatable.
- Normal reader HTTP requests never wait for Git networking.
- A manual command can force immediate content refresh.

### Phase 5: Publish reader state as one linear commit

1. If dirty markers are due, fetch immediately before constructing a commit.
2. Audit remote changes against the directory-ownership rules.
3. Use the fetched remote tip as the sole parent.
4. Build a candidate tree containing remote content/config and current local
   state without accumulating unpublished local commits.
5. Push exactly one fast-forward commit with compare-and-swap semantics.
6. If the remote advances, discard the candidate, fetch, rebuild, and retry up
   to three times.
7. Update the local ref and clear only the captured dirty markers after remote
   success is confirmed; regenerate the index for the accepted tree.

Default publication policy:

- Push after 30 seconds without a new state mutation.
- Push after five minutes even if mutations continue.
- Never push once per read marker.
- A push cycle also counts as the latest pull because it begins with fetch.

Exit criteria:

- Read, favorite, and comment changes survive a fresh clone.
- A remote content commit racing with state publication is preserved.
- A rejected push never loses local state or dirty markers.

### Phase 6: Surface conflicts for manual resolution

Stop automatically for:

- remote modification of Plainfeed-owned state;
- local modification of content/config by the application;
- invalid content or configuration;
- force-pushed or divergent `main` history;
- an unexpected ref, object-format, or repository-layout change;
- repeated compare-and-swap failure after three retries.

Record the reason, paths, local base, remote tip, and timestamp in
`.plainfeed/conflict.toml`. Add a reader banner and a `sync status` command.
Do not write Git conflict markers or choose a side automatically. The user
repairs the checkout or remote, clears the acknowledged report, and runs a
forced sync.

Exit criteria:

- Every blocked scenario has a deterministic fixture and recovery procedure.
- The reader remains available with the last valid local snapshot.

### Phase 7: Operational hardening

1. Test crashes at each file/ref transition.
2. Test concurrent state mutations during pull and push.
3. Test corrupt packs, timeouts, authentication failure, rate limits, and disk
   exhaustion.
4. Add bounded logs and a compact sync-status panel.
5. Verify native Git interoperability with `git fsck --full` after every
   end-to-end fixture.
6. Keep a host-native Git adapter as a recovery and diagnostic fallback.

## Trigger policy summary

| Trigger | Action |
| --- | --- |
| Deployment startup | Forced pull before serving |
| Host timer every 30 seconds | Local tick; network only when due |
| Last pull at least five minutes ago | Pull remote content |
| Dirty and idle for 30 seconds | Fetch, build one state commit, push |
| Dirty for at least five minutes | Force a batched state push |
| Manual sync | Forced complete cycle |
| GitHub webhook | Deferred; optional later optimization |

## Explicit non-goals

- General Git merge or rebase.
- Automatic conflict-marker generation or semantic content merging.
- Multiple Plainfeed instances writing the same `state/**` tree.
- Force pushes, tags, branch management, LFS, SHA-256 repositories, or signed
  pushes in the first adapter.
- A GitHub-only storage protocol.

If multiple uncoordinated content producers later need isolation, add producer
branches and a separate validating integrator. Do not add that complexity until
the single-branch ownership model is proven insufficient.
