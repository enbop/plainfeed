+++
format = "plainfeed.entry/v1"
id = "service-daemon-content"
title = "A single WASI service pulled this entry"
summary = "The Axum process owns both its HTTP listener and synchronization loop."
published = "2026-07-17T08:00:00Z"
channels = ["technology"]
tags = ["wasi", "axum", "sync"]

[source]
name = "Plainfeed service fixture"
url = "https://github.com/spore-bot/plainfeed"
+++

This entry is committed after the live checkout is cloned. The combined
Plainfeed service must fetch, validate, and activate it without an external
`plainfeed-sync tick` invocation.
