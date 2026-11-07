<!-- @doc
id = "ADR-0134"
title = "A page given as HTML loads from an origin of its own"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0134: A page given as HTML loads from an origin of its own

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0102](0102-page-api-for-browser-sources.md) decision 2 says which document gets
`window.jstream`. For a browser source given HTML text, it names "a `data:` document, as
the request policy's main-frame rule treats every `data:` document alike". That was true
when it was written: the helper loaded such a page as a `data:` URL, whose origin is
opaque.

An opaque origin had two problems (#1035, #347):

- Every `data:` document matched the rule, and so did a `data:` frame that a
  third-party frame made. The `local_network` opt-in for HTML pages reached those
  frames too (the HTML half of #721's gap).
- Chromium's Local Network Access refuses every local request from a page that isn't a
  secure context, so an HTML page that opted in couldn't reach this computer at all.

#1301 changed how these pages load. Each page given as HTML now loads from an `https`
origin of its own: `https://p<n>.html.jstream.invalid/`, where `<n>` is a number the
helper hands out once per page it opens. The helper's resource handler answers the
page's main-frame request to that URL with the source's HTML. `.invalid` is reserved
(RFC 2606), so no real host can be reached through that name. ADR-0102's text still
describes the old `data:` document. ADR-0102 is accepted, so its text can't change in
place (R-DOC-009), and this ADR records the change (#1306).

**Tier.** This is agent tier under [ADR-0043](0043-narrow-maintainer-tier.md) and
R-GOV-005. ADR-0102 is maintainer tier because it decides what engine data an untrusted
page may receive. This ADR leaves all of that as it was: the grant, its settings, what
`window.jstream` holds, and the rule that only the source's own document gets it. It
changes only which URL "the source's own document" is for one kind of source. The change
narrows the rule: one origin per page instead of every `data:` document.

<a id="decision"></a>
## Decision
This amends the third bullet of ADR-0102 decision 2, the one for a page given as HTML.
The rest of ADR-0102 is unchanged.

1. **A page given as HTML is the document at its own origin.** The helper loads it from
   `https://p<n>.html.jstream.invalid/`, an origin no other page shares. Only that
   page's main frame may load that URL (`RequestPolicy::html_page`). The request policy
   refuses every other request to a name under `html.jstream.invalid`, from any page or
   frame. Like a web page, the page gets `window.jstream` while its main frame shows a
   document of that origin, and never in a subframe or popup. A `data:` document is no
   longer the source's own document for any source.
2. **The page's `local_network` opt-in covers that origin only.** It works the same way
   as a web page's opt-in: the helper grants Chromium's local-network permissions to that
   origin alone, in the page's own request context. Frames the page embeds from other
   origins, and opaque frames, are refused.
3. **The page is a secure context.** Chromium refuses its plain `http` scripts and
   frames as mixed content, as it would for any `https` page. Loopback targets
   (`127.0.0.1`, `localhost`) are exempt.

<a id="consequences"></a>
## Consequences
- Each HTML page has its own storage, cookies and permissions, as a separate web origin
  would. Two HTML sources no longer share anything through their origin.
- An HTML page that loaded `http` scripts or frames from the web stops loading them.
  It has to use `https`, or inline them. The alpha.5 release notes say so.
- Where ADR-0102 decision 2 says "a `data:` document", read this ADR. The browser
  crate's README, the threat model and the `jstream-sources` descriptor already describe
  the new origin (#1301).
- The end-to-end tests in `crates/jstream-browser/tests/linux_page/local_network.rs`
  check three things on Linux: the page's origin, its opt-in, and that a third-party
  frame is refused. They haven't been run on Windows.
