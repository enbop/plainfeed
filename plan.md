# Plainfeed Plan

## Vision

Plainfeed is a personal, file-first information stream and reading server.
Automated agents and other producers collect or generate content on a schedule,
write it in a shared format, and let a lightweight web application provide the
reading experience.

The project is designed for one person first. It favors transparent files,
simple tools, and portability over multi-user infrastructure.

## Principles

- Files and directories are the source of truth; do not require a database.
- Keep all durable content and reader state human-readable and easy for an LLM
  to inspect and modify.
- Define and version the storage format before coupling producers to the server.
- Use Git for history, synchronization, and optional remote persistence.
- Support GitHub as a useful integration point without making it a runtime
  dependency or the only supported remote.
- Keep ingestion open: any agent or program that follows the file format can
  contribute content.
- Build the service in Rust and target WASI so it can run under Wasmtime.
- Optimize for a small, dependable personal service before adding scale or
  multi-user features.

## Initial Product Scope

The reader should eventually provide:

- A chronological information stream.
- Full content pages with source metadata and links.
- Automatic read/unread tracking.
- Favorites.
- Personal comments or notes on individual entries.
- Filtering by source, tag, date, and state.
- A file-backed representation for both content and reader state.
- Safe refresh and synchronization when files are changed by external agents.

## Proposed Repository Areas

The exact layout will be decided with the storage specification. A likely
separation is:

```text
content/       # Ingested entries, organized as immutable or append-oriented files
state/         # Read, favorite, and comment state
spec/          # Versioned file-format and compatibility documentation
crates/        # Rust workspace crates
web/           # Browser UI source and static assets
examples/      # Example feeds and producer output
tests/         # Cross-component fixtures and compatibility tests
```

Generated content and personal state may later live in a separate Git repository
from the application source. The format must work in either a single-repository
or split-repository deployment.

## Architecture Direction

### Storage protocol

Start by specifying stable identifiers, entry metadata, body representation,
directory layout, reader-state files, update rules, conflict behavior, and a
format version. Prefer Markdown plus a small, well-defined metadata format when
it remains unambiguous and round-trippable.

Content should be append-oriented where practical. Mutable reader state should
be isolated from source content to reduce Git conflicts and make automated
writers safer.

### Rust core

Keep parsing, validation, indexing, querying, and state transitions in a core
library with narrow filesystem abstractions. The core should not depend on HTTP,
GitHub, or a particular Git implementation.

### Server

Expose the reader and mutation operations through a small HTTP interface. The
WASI deployment model and available Wasmtime capabilities must be validated
early, especially filesystem access, sockets, clocks, and process execution.

### Web application

Choose the frontend framework after the HTTP and storage boundaries are clear.
Favor a small bundle, accessible reading layouts, keyboard navigation, and a
progressively enhanced interface that remains useful on low-powered devices.

### Git integration

Treat Git as a synchronization adapter around the file store. Begin with the
small subset needed by the product: inspect status, read history, stage changes,
commit, fetch, merge or rebase under a documented policy, and push.

Evaluate existing Rust Git implementations as a separate track. Do not let this
investigation block the initial reader, which can operate on an already checked
out directory.

The initial `gix`/WASI evaluation is recorded in
`docs/research/gix-wasi.md`. It supports the local repository direction after
small upstream compatibility fixes. A custom single-threaded gix HTTP
transport completes public fetches under Wasmtime, including pack/index
persistence and ref updates. A second experiment now completes a constrained
smart-HTTP push against a local `git-receive-pack` server: SHA-1, one branch,
one fast-forward commit, no deltas, and explicit status checking. Both paths
buffer complete HTTP messages, so they remain experimental options for bounded
personal repositories rather than the default synchronization implementation.
Keep a host adapter and a GitHub Git Database API adapter as fallback paths
until multi-commit synchronization, reconciliation, and broader failure cases
are verified. Authenticated single-commit GitHub push is now proven under
Wasmtime.

## Milestones

### 1. Specify the file format

Status: initial v1 slice implemented in `spec/v1.md` with fixtures. Tombstones
and cross-producer duplicate detection remain later extensions.

- Write the first version of the content and state specifications.
- Create representative fixtures for articles, short items, generated summaries,
  favorites, read markers, comments, edits, and deletion or tombstone behavior.
- Define producer requirements and forward-compatibility rules.

### 2. Build the Rust domain core

Status: initial parser, in-memory ordering, validation, and atomic state
transitions implemented in `plainfeed-core`.

- Parse and validate fixtures.
- Build an in-memory index from files.
- Query and sort entries.
- Apply state transitions with atomic file updates.
- Add compatibility and corruption-recovery tests.

### 3. Prove the WASI runtime

Status: initial `wasi:http/proxy` component verified with Wasmtime 43 using a
preopened writable `/data` directory.

- Compile the core and a minimal server target for the chosen WASI profile.
- Run it under Wasmtime with a mounted content directory.
- Document required runtime capabilities and deployment commands.

### 4. Deliver the reading experience

Status: minimal timeline, safe Markdown rendering, read tracking, favorites,
and comments implemented with server-rendered HTML and htmx.

- Implement the timeline and entry views.
- Add read, favorite, comment, and filtering interactions.
- Handle external file changes without losing local state.
- Make the interface responsive and keyboard accessible.

### 5. Add synchronization adapters

- Define safe commit boundaries and conflict policies.
- Implement or integrate the required Git subset.
- Test local-only, generic remote, and GitHub-backed workflows.
- Document how external AI agents can contribute without server-specific APIs.

## Open Decisions

- Stable identifier generation and duplicate detection across independent
  producers beyond the v1 syntax rules.
- Tombstones and explicit content deletion semantics.
- Comment edit and deletion history.
- Git implementation and merge strategy for concurrent automated writers.
- Whether content/state repositories are combined by default.

The v1 slice resolves the other initial choices: one Markdown file with TOML
front matter per entry, one TOML state file per entry, a WASI HTTP proxy
component, and server-rendered HTML progressively enhanced with htmx.

## Non-goals for the First Version

- Multi-user accounts and permissions.
- A hosted SaaS service.
- Large-scale search infrastructure.
- Mandatory GitHub APIs.
- A database-backed alternative.
- A general-purpose replacement for every RSS reader.
