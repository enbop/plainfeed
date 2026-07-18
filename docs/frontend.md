# Frontend decision

## Choice

Plainfeed v0.1 uses server-rendered HTML generated with
[Maud 0.27](https://maud.lambda.xyz/) and enhanced with
[htmx 2.0.10](https://htmx.org/docs/) for form mutations and channel fragment
navigation, plus a small plain JavaScript `IntersectionObserver` for automatic
read tracking. The htmx release file and license are vendored under
`web/vendor`; the application never loads a CDN at runtime.

## Why it fits

- The first product is a reading surface, not a general client application.
- The Rust server already owns parsing, state transitions, and HTML safety.
- htmx can replace one article after a favorite or comment mutation, so the
  browser does not need a duplicate domain model.
- Channel links progressively enhance from normal navigation to fast HTML
  fragment swaps with browser-history updates.
- Entry links progressively enhance into an in-page reading view while retaining
  a canonical, directly loadable `/entries/:id` URL.
- Opening an entry scrolls to the top of the reading surface. Its back control
  uses the htmx history snapshot when available, restoring the previous feed DOM
  and scroll position; directly loaded entries fall back to a normal home link.
- It is dependency-free in the browser and needs no Node-based build system.
- Server-rendered content remains readable if JavaScript is unavailable.

The timeline renders lightweight title-and-summary cards. Opening an entry swaps
the reading surface, updates browser history, and renders the stored Markdown
body; a direct request returns the same reader inside a complete document. Raw
HTML is removed and unsafe link destinations are replaced before the generated
body crosses a private trusted-HTML boundary into Maud. Producers place useful
navigation links in the Markdown body itself.
Application-specific JavaScript is intentionally limited to the behavior HTML
cannot express: marking an unread article after at least 60 percent of the card
has remained visible for 900 milliseconds.

Maud is isolated behind an HTTP-neutral `Renderer` interface. Routes construct
template-library-independent view models, while a reader builder selects the
renderer. A future renderer can replace Maud without changing storage, routing,
Markdown policy, or htmx response boundaries.

## Alternatives considered

- **Preact** is small and mature, but a component build and client state model
  would duplicate more of the server for this first slice.
- **Lit** is a good fit for reusable web components, but its package imports
  normally add a module resolution or bundling step that the current UI does
  not need.
- **No library** would work at the current size, but favorite and comment
  updates would require maintaining custom fetch-and-DOM replacement code.
- **Askama** provides strong compile-time templates and clean external HTML
  files, but Maud maps more directly onto the reader's small composable fragment
  functions and keeps the WASIp2 runtime footprint narrow.

## Upgrade boundary

The storage core and HTTP routes do not depend on Maud or htmx. Replace the frontend
when client-side navigation, offline queues, or complex multi-entry state makes
a client application materially simpler. Until then, keep HTML fragments as
the mutation response contract and avoid adding a JavaScript build chain.

Reader documents and fragments use `no-store`, while the embedded stylesheet,
application script, and vendored htmx asset are browser-cacheable for one hour.
This keeps navigation to separately rendered pages such as settings from
repainting before an already-loaded stylesheet is available.
