+++
format = "plainfeed.entry/v1"
id = "20260717-wasip2-reader"
title = "A file-backed reader running under Wasmtime"
published = "2026-07-17T00:30:00Z"
summary = "Plainfeed begins with a small end-to-end slice."
tags = ["plainfeed", "rust", "wasi"]
source = { name = "Plainfeed", url = "https://github.com/spore-bot/plainfeed" }
+++

Plainfeed treats files as the source of truth. An automated producer can add a
Markdown document, while the reader keeps mutable state in a separate TOML
file.

The first slice focuses on the reading loop: show a timeline, mark an item as
read, favorite it, and attach a personal comment.
