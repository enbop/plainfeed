# External producer contract

This contract lets an AI agent or another collector add Plainfeed entries
without coordinating through the reader server. It applies to the canonical
`main` branch of a Plainfeed data repository.

For a self-contained guide that can be copied into a data repository and used
directly as an AI task contract, see
[`PLAINFEED-CONTENT-GUIDE.md`](../PLAINFEED-CONTENT-GUIDE.md).

## Owned paths

External producers may create and correct files only under `content/**`.
Plainfeed owns `state/**`, the repository owner manages `config/**`, and the
synchronizer owns the ignored `.plainfeed/**` directory. A producer must not
stage, commit, delete, or rewrite paths outside `content/**`.

Each entry must satisfy [the v1 file format](../spec/v1.md). In particular:

- use UTF-8 Markdown with TOML front matter;
- assign a stable, globally unique entry ID and use it as the file stem;
- include a plain-text `summary` suitable for deciding whether to open the
  original article;
- choose one or more curated `channels` for feed navigation;
- keep `source.url` as the canonical link to the original;
- never reuse an entry ID for unrelated content.

A producer may correct an entry it originally created. Independent producers
must not edit the same entry ID. Removing a content file removes it from the
feed; v1 intentionally leaves any corresponding reader state orphaned.

## Git write procedure

1. Fetch `main` over HTTPS and require a fast-forward update.
2. Start from the fetched remote tip, not from an older unpublished commit.
3. Write complete entry files using a temporary file and atomic rename.
4. Validate every new or changed entry against `plainfeed.entry/v1`.
5. Audit the staged paths and require all of them to be under `content/**`.
6. Create one focused commit and push it as a fast-forward update.
7. If the remote advanced, discard the candidate commit, fetch the new tip,
   reapply the content-only change, validate again, and retry.

Never force-push, merge divergent history automatically, commit credentials,
or store a token in repository configuration. A rejected push or ownership
audit is a stopped operation, not permission to choose a winning version.

## Minimal handoff fields

An automated producer should report the resulting commit ID, added or changed
entry IDs, and any validation or push error. This gives the synchronizer and a
human enough information to audit an update without a provider-specific API.
