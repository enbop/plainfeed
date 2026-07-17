# Agent guidance

## Working agreements

- Check the current branch and `git status`, and preserve unrelated changes.
- Read `plan.md` before making architectural or storage-format changes.
- Keep changes focused on the requested scope.
- Do not commit, push, or open a pull request unless explicitly asked.
- Use English for code, documentation, commit messages, and repository metadata.

## Project constraints

- Files and directories are the source of truth; do not introduce a database.
- Keep durable content and reader state human-readable and suitable for direct
  editing by people, LLMs, and small producer programs.
- Treat the versioned file format as a public protocol. Document incompatible
  changes and update fixtures and compatibility tests with the specification.
- Keep content separate from mutable reader state where practical.
- Keep the core independent of GitHub and any specific Git hosting provider.
- Keep parsing, indexing, and state logic independent of the HTTP and UI layers.
- Maintain WASI compatibility. Isolate platform-specific code behind narrow
  interfaces and verify behavior under Wasmtime when runtime code changes.
- Avoid adding dependencies without a clear benefit to the personal, lightweight
  deployment goal.

## Verification

Run checks relevant to the changed code. Once the Rust workspace exists, the
default checks are:

```bash
cargo fmt --all -- --check
cargo build --workspace
cargo test --workspace
```

For changes that affect the runtime boundary, also build the selected WASI
target and run the relevant Wasmtime smoke test documented by the project.

## Commits

Use Conventional Commits in English:

```text
<type>(<optional-scope>): <imperative summary>
```

## AI attribution

For non-trivial AI-assisted commits, add:

```text
Assisted-by: <agent>
```

Include the model only when it is explicitly known:

```text
Assisted-by: <agent>:<model>
```

## Pull requests

Before drafting or creating a pull request, read
`.github/PULL_REQUEST_TEMPLATE.md` if it exists and use it as a reference.
