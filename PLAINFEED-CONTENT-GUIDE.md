# Plainfeed AI content producer guide

This file is a self-contained contract for an AI agent or automated research
task that writes articles into a Plainfeed content repository. Copy it to the
root of each content repository and tell the producer to read it before every
run.

The producer's job is to research, write, and update Plainfeed entry files. It
does not manage reader state or Plainfeed itself.

## Non-negotiable repository boundary

Create or correct files only under `content/**`.

Do not create, edit, delete, stage, or commit files under:

- `state/**`, which belongs to the reader;
- `config/**`, which belongs to the repository owner;
- `.plainfeed/**`, which is local synchronization metadata;
- application source, workflow, or repository-administration paths.

Treat the repository as append-oriented. Do not delete an existing article or
rewrite another producer's article unless the task explicitly identifies that
article as yours and asks for a correction. Never reuse an entry ID for a
different subject.

If a requested operation requires changes outside `content/**`, stop and
report what the repository owner needs to do.

## Required file location

Store each article at:

```text
content/YYYY/MM/<entry-id>.md
```

Use the year and month from the entry's `published` timestamp. The filename
without `.md` must exactly equal the front-matter `id`.

Entry IDs must:

- contain 1–128 characters;
- begin with a lowercase ASCII letter or digit;
- contain only lowercase ASCII letters, digits, and hyphens;
- remain stable across later corrections;
- be unique across the repository.

A useful pattern is `YYYYMMDD-<short-topic-slug>`, adding a stable source ID or
another meaningful suffix when needed. Search the existing repository before
choosing an ID.

## Exact entry format

Every entry is UTF-8 Markdown with TOML front matter. Both delimiter lines must
contain exactly `+++`:

```markdown
+++
format = "plainfeed.entry/v1"
id = "20260718-example-topic"
title = "A concise, informative article title"
published = "2026-07-18T09:30:00Z"
summary = "A plain-text summary that helps the reader decide whether to open the article."
tags = ["example", "research"]
channels = ["technology"]
source = { name = "Primary publication", url = "https://example.com/original-item" }
+++

Write the complete article here in Markdown.

Use [absolute links](https://example.com/reference) where the reader may want
to inspect a source or continue reading.
```

Required front-matter fields:

- `format`: exactly `plainfeed.entry/v1`.
- `id`: stable unique ID matching the filename.
- `title`: plain-text display title; do not put Markdown in it.
- `published`: RFC 3339 timestamp with an explicit offset, such as `Z` or
  `+09:00`.
- `source.name`: human-readable name of the source or producing publication.
- `source.url`: absolute canonical `https://` URL for the source item.

Expected fields for automated articles:

- `summary`: one short plain-text paragraph. Do not use Markdown, citations, or
  a duplicate of the title.
- `tags`: a small array of descriptive terms. Prefer stable lowercase tags.
- `channels`: one or more navigation channel IDs. Read
  `config/channels.toml` when it exists and prefer its established IDs, but do
  not edit that file.

Channel IDs may contain lowercase ASCII letters, digits, and hyphens. A `/`
may separate valid segments, for example `projects/plainfeed`. Do not put
spaces, uppercase letters, empty segments, leading slashes, or trailing
slashes in channel IDs.

When correcting an existing entry, preserve its ID and any unknown
front-matter fields.

## Source and timestamp policy

For a summary of one external item:

- use the original item's publication time for `published` when reliably
  known; otherwise use the time the entry was produced;
- use the original publication as `source.name`;
- use the canonical original item URL as `source.url`.

For a briefing synthesized from multiple sources:

- use the briefing generation time for `published`;
- use the strongest primary or anchor source for both `source.name` and
  `source.url`;
- make it clear in the body that the article is a multi-source synthesis;
- link every material source at the relevant point in the body;
- optionally finish with a short `## Sources` list when that improves
  auditability.

Do not invent a URL, publication date, quotation, or attribution. If a fact is
uncertain or sources disagree, say so in the article.

## Article-writing rules

Write a useful standalone article, not merely a link dump or a repetition of
the summary.

- Put the important conclusion and context near the beginning.
- Use headings and lists when they improve scanning.
- Put links directly on the claims or references they support.
- Use absolute `https://` or `http://` destinations. Relative URLs are not
  suitable for imported content.
- Paraphrase sources and keep quotations short. Do not copy an article in
  full.
- Separate sourced facts from the producer's inference or synthesis.
- Do not include secrets, access tokens, private prompts, or internal task
  metadata.
- Do not emit raw HTML. Plainfeed removes raw HTML during rendering.

Plainfeed supports ordinary CommonMark plus tables, footnotes,
strikethrough, and task lists. Use conventional Markdown such as
`**bold text**`; spaces immediately inside the markers may prevent emphasis
from being recognized.

## Duplicate and correction policy

Before creating an entry, inspect existing `content/**` files for:

1. the same canonical `source.url`;
2. the same external item or announcement linked in an existing body;
3. an existing ID or substantially equivalent title and subject.

If the item already exists, do not create a second article merely because the
scheduled search found it again. Either make no change or correct the existing
entry only when the task authorizes corrections and the new information
materially improves it.

Recurring briefings are distinct entries only when they cover a new period or
materially new developments. Encode that period or date in the ID and title.

## Procedure for each automated run

1. Read this file and the current task instructions.
2. Refresh repository context and inspect `config/channels.toml` if present.
3. Search existing `content/**` for duplicates and ID collisions.
4. Research the requested subject using reliable, preferably primary sources.
5. Choose the canonical source, timestamp, channels, tags, and stable ID.
6. Write one complete entry file in the required calendar directory.
7. Re-read the rendered Markdown conceptually and validate the checklist below.
8. Review the Git diff and confirm every changed path is under `content/**`.
9. Follow the task's requested Git workflow. Never force-push or resolve a
   conflict by discarding remote work.
10. Report the changed entry IDs, paths, sources, and any uncertainty or error.

If the remote branch advances while publishing, refresh it, reapply the
content-only change, recheck duplicates and paths, and retry without rewriting
unrelated work.

## Final validation checklist

Before publishing, verify all of the following:

- [ ] Every changed path is under `content/**`.
- [ ] The path is `content/YYYY/MM/<entry-id>.md`.
- [ ] The filename stem exactly matches `id`.
- [ ] `format` is exactly `plainfeed.entry/v1`.
- [ ] The ID syntax is valid and does not collide with an existing entry.
- [ ] `published` is valid RFC 3339 with an explicit offset.
- [ ] TOML strings and arrays are syntactically valid.
- [ ] The summary is concise plain text.
- [ ] Channel IDs are valid and preferably come from the existing catalog.
- [ ] `source.url` is a real canonical absolute URL.
- [ ] Material factual claims have appropriate links in the body.
- [ ] The entry is not a duplicate of existing content.
- [ ] No secrets, raw HTML, reader state, or unrelated files are included.

## Minimal task prompt

A scheduled task can use this instruction:

> Read `PLAINFEED-CONTENT-GUIDE.md` in the repository root and follow it as the
> content contract. Research the requested topic, check existing entries for
> duplicates, and add only valid Plainfeed articles under `content/**`. Include
> source links in the Markdown body and report the files and entry IDs changed.
