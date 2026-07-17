# Frontend decision

## Choice

Plainfeed v0.1 uses server-rendered HTML with
[htmx 2.0.10](https://htmx.org/docs/) for form mutations and channel fragment
navigation, plus a small plain JavaScript `IntersectionObserver` for automatic
read tracking. The htmx release file and license are vendored under
`web/vendor`; the application never loads a CDN at runtime.

## Why it fits

- The first product is a reading surface, not a general client application.
- The Rust server already owns parsing, state transitions, and HTML safety.
- htmx can replace one entry card after a favorite or comment mutation, so the
  browser does not need a duplicate domain model.
- Channel links progressively enhance from normal navigation to fast HTML
  fragment swaps with browser-history updates.
- It is dependency-free in the browser and needs no Node-based build system.
- Server-rendered content remains readable if JavaScript is unavailable.

The timeline renders title, summary, tags, and a source link rather than the
stored full body. Application-specific JavaScript is intentionally limited to
the behavior HTML cannot express: marking an unread entry after at least 60
percent of the card has remained visible for 900 milliseconds.

## Alternatives considered

- **Preact** is small and mature, but a component build and client state model
  would duplicate more of the server for this first slice.
- **Lit** is a good fit for reusable web components, but its package imports
  normally add a module resolution or bundling step that the current UI does
  not need.
- **No library** would work at the current size, but favorite and comment
  updates would require maintaining custom fetch-and-DOM replacement code.

## Upgrade boundary

The storage core and HTTP routes do not depend on htmx. Replace the frontend
when client-side navigation, offline queues, or complex multi-entry state makes
a client application materially simpler. Until then, keep HTML fragments as
the mutation response contract and avoid adding a JavaScript build chain.
