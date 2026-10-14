<!-- @doc
id = "ADR-0034"
title = "YouTube interaction scope v2"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0034: YouTube interaction scope v2

Supersedes [ADR-0009](0009-youtube-rtmps-and-api.md). It restates all of ADR-0009's
decision, which stands, and widens what JStream does with the YouTube API: sending chat
as the channel, showing a chat message on stream, and audience stats.

<a id="context"></a>
## Context
Streaming should work with just a stream key. Agents also benefit from managing the
broadcast itself: title, schedule, health and chat. [ADR-0009](0009-youtube-rtmps-and-api.md)
decided this: a generic RTMP/RTMPS output, plus optional OAuth and the
[YouTube Live API](https://developers.google.com/youtube/v3/live/getting-started) for
creating and scheduling broadcasts, binding streams, setting metadata, and reading stream
health and live chat. [ROADMAP › M3](../../ROADMAP.md#m3) builds `jstream-youtube`.

The chosen UI prototype (`design/ui-prototype/`, direction D) goes further. Its chat panel:
- **sends chat as the channel** ("Chat as JStream…");
- offers **"Show this message on stream"**, a featured message drawn on the program;
- shows viewers and likes in the stream stats.

The UI decision register
([F. Protocol and clients](../../design/ui-prototype/decision-register.md#f-protocol-and-clients),
entry D40) records
all three as outside ADR-0009's scope:
- Sending chat is a **write** to the channel, so it needs a write OAuth scope and costs
  API quota per message.
- "Show on stream" needs a source that renders the message.
- Polling viewers and likes costs quota alongside chat polling.

Widening a product scope, and superseding an ADR, are maintainer decisions
([ADR-0017](0017-adr-decision-tiers.md)). The maintainer decided D40 in the builder
session. The maintainer's choice was **"Yes, in M3"**.

<a id="decision"></a>
## Decision

**Unchanged from ADR-0009:**
- A generic RTMP/RTMPS output (with enhanced RTMP for HEVC/AV1) works with any service,
  and reconnects with backoff.
- `jstream-youtube` adds optional Google OAuth and the YouTube Live API: creating and
  scheduling broadcasts, binding streams, setting metadata, and reading stream health and
  live chat.
- All of it is exposed as protocol edits, actions and queries, and so over MCP.
- An `Rtmp` output may be bound to a YouTube broadcast.

**New in this ADR:**

**1. Send chat as the channel.**
- An action `youtube.chat.send { text }` posts a message to the bound broadcast's live
  chat, as the signed-in channel (`liveChatMessages.insert`).
- It is an action: not undoable, and not batchable. A message, once sent, is public.
- Every send goes to the audit log with the client's name, and the chat panel marks
  messages sent by an agent.
- Agents may send, like every other capability ([ADR-0006](0006-mcp-full-access.md)).

**2. Show a chat message on stream.**
- An action `youtube.chat.feature { message_id }` (or none, to clear) sets the
  **featured message**. It is runtime state, not part of the collection, and is reported
  by an event.
- A new native source kind, **chat message**, renders the featured message: author name,
  badge, text, and Super Chat amount, with styling settings (font, colours, background,
  padding, maximum width). It renders nothing while no message is featured. It sits in
  scenes like any other source.
- Message text is untrusted and is drawn as plain text: no markup and no links.
- It is a native source rather than a local page in a browser source
  ([ADR-0021](0021-browser-source.md)), so no page needs access to the control API to
  get the message.

**3. Audience stats.**
- `jstream-youtube` polls the bound broadcast's **concurrent viewers** and **likes**, and
  derives the **chat rate** from the chat it already polls.
- They are reported in a low-rate `youtube.stats` event, and in a query.
- They are reads only. JStream does not moderate chat (delete, ban, time out) in this
  scope; that would need another ADR.

**4. OAuth scopes.**
- The read and manage features of ADR-0009 use
  `https://www.googleapis.com/auth/youtube`.
- **Sending chat adds** `https://www.googleapis.com/auth/youtube.force-ssl`, a write scope
  on the channel.
- JStream asks for it **incrementally**: only when the user first turns on chat sending,
  not at sign-in. A user who never sends chat never grants it.
- The exact scope each API method accepts is re-checked against the API reference when
  `jstream-youtube` is built. If one scope turns out to cover everything, JStream asks
  for the narrowest set that works, and still asks for write access only when sending is
  turned on.
- Tokens stay in the OS credential store ([R-SEC-001](../RULES.md#R-SEC-001)).

**5. Quota.**
- Chat is polled at the interval the API returns, never faster.
- Stats are polled at a low fixed rate.
- The engine tracks the day's estimated quota use. When it runs low, stats polling slows
  or stops first, then chat polling. Sending and broadcast control keep working as long
  as there is quota.
- Quota errors are surfaced as protocol errors, as in ADR-0009.

**6. Timing.** Written now, **built in M3** with `jstream-youtube`.

<a id="consequences"></a>
## Consequences
- **Unchanged from ADR-0009:** the app needs a Google Cloud OAuth client, and token
  storage in the OS keychain.
- **The consent screen grows** for users who send chat: the write scope lets JStream post
  as the channel. Google requires verification of OAuth clients that use sensitive
  scopes, so a release needs a verified client, or else users see the unverified-app
  warning.
- **Agents can speak as the channel in public.** That is a deliberate consequence of
  ADR-0006's full access. The audit log and the "sent by an agent" marker are the
  safeguards, and there is no extra permission tier.
- **Quota pressure** rises with stats and sending. The degradation order keeps the stream
  and broadcast control working.
- **A new source kind** (chat message) joins the source list, with its text rendering
  shared with the text source.
- **Follow-on edits when this ADR is accepted** (not made while it is proposed):
  - ADR-0009: `status = "superseded"`, `superseded_by` pointing here.
  - Re-point every reference to ADR-0009 here, since references to a superseded ADR fail
    the docs check: the `depends_on` and links in `crates/jstream-mux/README.md` and
    `crates/jstream-outputs/README.md`.
  - [ARCH › Crate map](../ARCHITECTURE.md#crate-map): `jstream-youtube` gains chat
    sending and stats, and `jstream-sources` the chat message kind.
  - [ARCH › Control protocol](../ARCHITECTURE.md#protocol): the send and feature actions,
    and the `youtube.stats` event.
  - [ROADMAP › M3](../../ROADMAP.md#m3): the `jstream-youtube` bullet gains sending chat,
    the featured message, and audience stats.
  - `docs/external.toml`: the `yt-live-api` entry's `relied_on` gains
    `liveChatMessages.insert`, the video statistics reads, and the scopes.
