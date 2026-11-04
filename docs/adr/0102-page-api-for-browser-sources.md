<!-- @doc
id = "ADR-0102"
title = "Browser source pages can opt in to reading the engine through window.jstream"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0102: Browser source pages can opt in to reading the engine through window.jstream

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
HTML pages in browser sources can't see the engine. Pages that should move together each
read `Date.now()`, no page knows when its item was shown or which scene is on program,
data pages fake their data, and audio visualisers simulate the mix (#1063, #1060).
Native motion is getting one engine animation clock, a media-clock time that every motion
modifier and timeline reads (#1058). A page that draws to that same clock stays in step
with native motion, and with the other pages.

Pages are untrusted content ([ADR-0021](0021-browser-source.md),
[THREAT-MODEL › Browser source](../security/threat-model.md#browser)). They run in a
sandboxed helper process ([ADR-0048](0048-sandboxed-browser-helper.md),
[ADR-0095](0095-browser-helper-on-linux.md)) that the engine drives over a private
pipe. Today nothing on that pipe reaches a page's JavaScript. A page served from the web can
send anything it reads to its own server, so whatever the engine gives a page may leave the
machine.

**Tier.** This ADR is maintainer tier under [ADR-0043](0043-narrow-maintainer-tier.md),
on two grounds:
- **Security and privacy posture.** It decides which engine data an untrusted page that can
  reach the network may receive, and so what may leave the machine. That includes a bus's
  spectrum, which no control client gets today (`meters.tick` carries only levels), and
  preview scene names. [ADR-0021](0021-browser-source.md) set a limit ("no access to the
  JStream control API unless the user grants it to that source"). It did not decide what a
  grant exposes, how finely it is split, or what the user is warned about.
- **A public compatibility promise.** Decision 9 promises page authors that
  `window.jstream` keeps its members across versions.

ADR-0021's own control opt-in stays unbuilt. Building it would need its own decision, and
it must never hand a page the control token (#347).

<a id="decision"></a>
## Decision
1. **Opt in per source, off by default.** The `browser` kind gains four settings
   ([ADR-0025](0025-typed-settings-kind-descriptors.md)), all off or empty in a new
   source and in every existing one:
   - `page_api` (`bool`). Turns the API on, with the clock and the page's own
     visibility (decisions 4 and 5). With it off, the page has no `window.jstream`, and
     the engine sends its helper nothing about the page beyond what it sends today.
   - `page_api_scenes` (`bool`). Adds the program and preview scenes (decision 6).
   - `page_api_values` (`string`, at most 1 KiB). The shared values the page may read
     (decision 7), as comma-separated names or prefixes ending in `.*`.
   - `page_api_bus` (the existing `bus` reference parameter type, `ParamType::Bus` since
     protocol 0.24, as the `waveform` and `spectrum` kinds use; `null` for none).
     The one bus whose levels the page may read (decision 8).

   The last three apply only while `page_api` is on, and the UI shows them under it.
   Changing any of them is an ordinary undoable settings edit, which only a control client
   can make. Nothing a page does can change them. The Properties dialog puts the grant
   in its own group and says what each setting gives the page. Beside `page_api_bus` it
   says: "A page given a bus can follow the shape of its sound, and a page from the web
   can send that anywhere. Give it only the bus it shows."
2. **Only the source's own document gets it.** The API is defined in the page's main
   frame, before any page script runs, and only while that frame shows the source's own
   document:
   - a web page: a document of the same origin as the source's URL. Same origin means
     every path on that host and port, so a source whose URL is on a shared host grants
     every page on that host;
   - a local file: a `file:` document the request policy lets the page load, in the
     page's folder or below it;
   - a page given as HTML: a `data:` document, as the request policy's main-frame rule
     treats every `data:` document alike;
   - a blank page (no URL and no file): never.

   It is never defined in a subframe or a popup, or after the main frame navigates to a
   document outside that rule. The helper's render-process handler decides this when each
   JavaScript context is created. It checks the main frame's URL against the source's
   location, which the engine sends when it opens the page. This is the same rule
   `local_network` uses for web pages (#721), applied to the main frame only.
3. **One-way and read-only.** Data flows from the engine to the helper (a new
   `ToHelper::PageData` message on the pipe in `crates/jstream-browser/src/wire.rs`), then from the helper's
   browser process to the page's renderer as a CEF process message, then into
   `window.jstream`. No message from a page reaches the engine. The helper's
   browser-process handler drops every process message a renderer sends. The API has no
   method that sends anything, and the page never sees the control token or any command.
   The engine filters each update to the page's grant before sending it.
   - **Renderer isolation.** Each page with `page_api` on gets a request context of its
     own. Chromium never shares a renderer process across request contexts, so a
     compromised renderer holds only what its own source was granted. The implementation
     PR checks this on a run. Until that check passes, the threat model says "only what
     the sources sharing its renderer were granted".
   - **Rate.** Updates are coalesced to at most one per page frame (the source's
     frame-rate cap, at most 60 a second). An update larger than the pipe's limit spans
     several messages.
   - **Size.** An update that would exceed the pipe's message limit (`MAX_MESSAGE_BYTES`)
     is split across consecutive messages, each value whole in one of them. This covers
     the on-load catch-up and a burst of value changes in one frame. A value is at most
     16 KiB (decision 7), so it always fits.
4. **The clock.** `jstream.clock.now()` returns the engine animation clock in seconds:
   the same time that native motion and timelines read (#1058). The engine sends an
   anchor (a clock time) when the page opens, every 250 ms after that, and whenever the
   clock jumps. The page counts forward from the latest anchor with `performance.now()`.
   The anchor includes a lead: the page's measured delay from paint to composite, so
   what a page draws for time *t* shows on program at *t*. The engine estimates the lead
   as the page's frame interval plus the mean age of its frames when the compositor
   samples them. A page that draws from `now()` agrees with native motion to within a
   program frame, and #1061's frame strips check that.
5. **The page's own visibility.** `jstream.source` reports whether any item showing
   this source is visible on program, and on preview, nested scenes included. It also
   gives the clock time each of those last became true (`shownAt`, `previewShownAt`, or
   `null` if never). `shown` and `hidden` events fire as they change. This is per source,
   since several items can place one source
   ([ADR-0023](0023-placements-references-and-clones.md)). A `restart_on_show` reload
   (#1057) happens before the new document's `shownAt` is set.
6. **Scenes.** With `page_api_scenes` on, `jstream.scenes` holds the program scene and
   the preview scene (`null` outside studio mode), each as `{ id, name }`, and a
   `scenes` event fires when either changes.
7. **Shared values.** A new studio-level store of named values that pages and
   (later) timelines and text sources read. Each value is set over the protocol and MCP:
   - **Commands.** `values.set { name, value }` (`value: null` removes it), `values.list`,
     and a `values.changed` event. The MCP `values` tool (ops `values.set`,
     `values.list`, per [R-API-002](../RULES.md#R-API-002)). They are **actions**, not
     edits ([R-ARCH-006](../RULES.md#R-ARCH-006), [ADR-0013](0013-command-model.md)): a
     value that changes often must not fill the undo history.
   - **Storage.** The engine holds values in memory. They are not collection state, not
     saved, and not `rev`. A collection that wants a value at start sets it from a
     timeline cue (#1059) or from the agent that builds the scene.
   - **Names and limits.** A name is dotted lowercase segments (`status.text`,
     `counter.a`) of `[a-z0-9_]`, at most 64 bytes. A value is any JSON value, at most
     16 KiB serialized, with at most 256 names in the store. A set beyond a limit is refused
     with `invalid_argument`.
   - **What the page sees.** `jstream.values.get(name)` returns the value. A `value`
     event fires with `{ name, value }` for each change the page's `page_api_values`
     covers, and on load the page gets every covered value it doesn't have yet. Values
     reach the page as JSON text that the API parses, and are never evaluated as code.
   - **The name.** These are not [ADR-0032](0032-live-edits-and-undo.md)'s live values,
     which are a gesture's uncommitted edits. The protocol and the docs call them shared
     values to keep the two apart. The native-motion ADR (ADR-0099, #1074) has a closed
     cue set limited to actions, never edits. This ADR clarifies that set's "set a live
     value" cue as `values.set` on this store: an ADR-0032 live value is an edit, so the
     cue can't mean one. This ADR adds no cue.
8. **Audio levels.** With `page_api_bus` set, `jstream.levels()` returns that bus's
   latest reading, and a `levels` event fires for each one. A reading has its post-fader
   `peak` and `rms` per channel, as `meters.tick` reports them, and `bands`, the bus's
   spectrum from #1060's meter tap (the band count and spacing that tap fixes).
   Readings arrive at most once per page frame. A bus that is deleted, or a setting that
   names no bus, gives `null`. The page never gets samples.
9. **The API's shape.** `window.jstream` is a frozen object with `version: 1`, the
   members above, and `addEventListener` and `removeEventListener` for its events. A
   member whose grant is off is absent, so a page can test for it. Later versions add
   members and keep these ones (`version` goes up), and a page written for `1` keeps
   working.
10. **Where the code lives.** The value store and per-page feeds are in `jstream-core`.
    `values.*` is in `jstream-protocol`, with the MCP tools
    in `jstream-mcp`. The four settings go in `jstream-sources`' browser kind. The pipe
    message, the render-process handler and the JavaScript shim go in `jstream-browser`.
    The build follows the motion work, in three PRs: the value store and its tools
    (a protocol slot); the page API with the clock, visibility and scenes, once #1058's
    clock merges; and audio levels, once #1060's meter tap merges.

<a id="consequences"></a>
## Consequences
- HTML monitors and visualisers can draw from the same clock as native motion, show
  data an agent sets over MCP, and react to the real mix, rather than faking their data
  (#1063, #1060).
- A page that is granted data can send it off the machine, if it is a web page or loads
  one. That is the user's choice, made per source and per kind of data, and the Properties
  dialog says so. Two grants deserve the warning most:
  - **Preview scene names.** These can name what the user hasn't shown yet.
  - **A bus's bands.** Band levels at up to 60 a second follow a sound's shape closely
    enough to make speech on that bus partly intelligible. Granting a microphone bus is
    close to letting the page hear a coarse copy of it.
- A compromised renderer gets only what its source was granted, and still has no path to
  the engine. The threat model's browser section records these controls when they are
  built, and the page API's gaps, if any, become issues there.
- Shared values are lost when the engine restarts. A scene that needs a value at start
  sets it itself. Saving values with the collection could come later, as its own
  decision.
- Each granted page costs at most one update per frame, usually one small pipe message
  (a large catch-up spans several), and none when nothing changed except the 250 ms clock
  anchor.
- The protocol grows by `values.*` only. That is additive under
  [R-API-001](../RULES.md#R-API-001) and goes through the protocol merge queue.
- Timelines (#1059) set shared values through ADR-0099's "set a live value" cue, which
  uses this store rather than a store of their own.
