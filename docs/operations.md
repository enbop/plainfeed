# Synchronization operations

## Normal checks

Run `plainfeed-sync status` without network permissions. The output is a stable
line-oriented `plainfeed.sync-status/v1` record containing the last pull/push,
the bounded last error, dirty-marker count, conflict details, and whether a
push recovery transaction is pending. The reader header shows the same state
at a glance as `synced`, `local changes pending`, `sync delayed`, `sync
recovery pending`, or `sync paused`.

The adapter bounds each Git HTTP response and generated push pack to 64 MiB,
the local object database to 256 MiB, each reachable object to 16 MiB, and a
fetched or pushed snapshot to 100,000 objects. Connection setup is limited to
20 seconds and a complete HTTP request to 30 seconds. Persisted command errors
are truncated to 4096 characters. Authentication,
timeouts, HTTP errors, corrupt protocol responses, repository growth, and file
system errors fail the cycle without activating fetched files or clearing
dirty markers.

## Recovery matrix

| Symptom | Safe action |
| --- | --- |
| `conflict_active=true` | Repair the reported ownership, validation, or history problem; run `acknowledge-conflict`; then run `force`. |
| `pending_push_active=true` | Run `force` with the same remote and credentials. Plainfeed fetches first and confirms whether the previous push was accepted. |
| Reader remains HTTP 503 and `.plainfeed/update.lock/` remains after a killed sync process | Stop scheduled sync invocations, run `recover-local`, inspect the restored feed, then run `force`. |
| Authentication, rate-limit, timeout, or offline error | Keep the checkout unchanged, correct credentials or wait for service recovery, then run `force`. |
| Repository/disk limit | Free space or reduce repository history outside Plainfeed; validate with native Git before retrying. |

`recover-local` is intentionally explicit because removing another live sync
process's lock would be unsafe. It either recognizes that the current checkout
already matches `main` and removes stale backup data, or restores the saved
content/config snapshot and validates it against the unchanged local ref.

## Native Git diagnostic fallback

Keep native Git available on the host as a diagnostic and manual-repair path;
the WASI service does not depend on it. Stop the scheduler and reader mutations
before using it:

```sh
git -C /path/to/plainfeed-data status --short
git -C /path/to/plainfeed-data fsck --full
git -C /path/to/plainfeed-data log --graph --decorate --oneline --all -20
git -C /path/to/plainfeed-data diff main -- content config state
git -C /path/to/plainfeed-data fetch origin main
```

The checked-in helper packages the non-destructive inspection and fetch subset:

```sh
scripts/plainfeed-native-git.sh inspect /path/to/plainfeed-data
scripts/plainfeed-native-git.sh fetch /path/to/plainfeed-data origin
```

`fetch` writes only `refs/remotes/plainfeed-recovery/main`; neither action
updates the worktree, local `main`, or remote repository. It reports whether a
manual fast-forward is possible and lists changed managed paths.

Never place a PAT in the remote URL, Git configuration, shell history, or the
data checkout. Manual history repair is deliberately not automated: inspect
the conflict, preserve `state/**`, produce one valid linear `main`, then use the
acknowledge-and-force flow.

## Regression suites

- `cargo test --workspace` covers formats, ownership, bounds, atomic metadata,
  activation rollback, repository contracts, and scheduling policy.
- `scripts/verify-sync-command-wasmtime.sh` covers scheduling, offline errors,
  conflict acknowledgement, and interrupted activation recovery.
- `scripts/verify-local-recovery-wasmtime.sh` isolates interrupted activation
  recovery without requiring network access.
- `scripts/verify-state-race-wasmtime.sh` covers successful CAS retry and
  retry exhaustion.
- `scripts/verify-pending-push-recovery-wasmtime.sh` drops a successful push's
  HTTP response and proves idempotent recovery.
- `scripts/verify-concurrent-state-push-wasmtime.sh` mutates reader state while
  push completion is delayed and proves the new dirty marker survives.
- `scripts/verify-network-failures-wasmtime.sh` injects an HTTP 429, a truncated
  upload-pack response, and a request timeout, then proves the next forced
  cycle recovers without premature activation.
- `scripts/verify-state-push-github-wasmtime.sh` performs the real GitHub
  HTTPS/PAT push and verifies a fresh SSH clone plus `git fsck --full`.
- `scripts/verify-conflict-fixtures-wasmtime.sh` covers remote state,
  unexpected paths, invalid content, missing `main`, divergent history, and
  their manual recovery procedures.
