<!-- @doc
id = "ADR-0041"
title = "YouTube interaction scope v3: one sign-in"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0041: YouTube interaction scope v3: one sign-in

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

Supersedes [ADR-0034](0034-youtube-interaction-scope-v2.md). It restates all of
ADR-0034's decision, which stands, except §4: JStream asks for the full `youtube` scope
in a single sign-in instead of asking for it incrementally.

<a id="context"></a>
## Context
[ADR-0034](0034-youtube-interaction-scope-v2.md) §4 has JStream sign in with
`youtube.readonly` and ask for `youtube` **incrementally**, the first time the user uses
broadcast control or turns chat sending on.

Building `jstream-youtube` (roadmap Y1) found two things:
- Google's guide to OAuth 2.0 for installed apps (the `google-oauth-native-app` entry in
  `docs/external.toml`) says incremental authorization is not supported for installed
  apps. "Incrementally" would therefore mean a second, complete sign-in later, not
  Google's incremental flow.
- JStream's main use is going live, which needs broadcast control, so almost every user
  would see that second consent screen soon after the first.

The maintainer asked for one sign-in with the full scope instead: "We should avoid the
incremental authorization and just have the single authorization with full scope".
What JStream asks Google to let it access is security and privacy posture, so this is a
maintainer decision ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision

**Unchanged from ADR-0009 (as restated by ADR-0034):**
- A generic RTMP/RTMPS output (with enhanced RTMP for HEVC/AV1) works with any service,
  and reconnects with backoff.
- `jstream-youtube` adds optional Google OAuth and the YouTube Live API: creating and
  scheduling broadcasts, binding streams, setting metadata, and reading stream health and
  live chat.
- All of it is exposed as protocol edits, actions and queries, and so over MCP.
- An `Rtmp` output may be bound to a YouTube broadcast.

**Unchanged from ADR-0034, except §4:**

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
- Message text and author names are untrusted and are drawn as plain text: no markup
  and no links.
- It is a native source rather than a local page in a browser source
  ([ADR-0021](0021-browser-source.md)), so no page needs access to the control API to
  get the message.

**3. Audience stats.**
- `jstream-youtube` polls the bound broadcast's **concurrent viewers** and **likes**, and
  derives the **chat rate** from the chat it already polls.
- They are reported in a low-rate `youtube.stats` event, and in a query.
- They are reads only. JStream does not moderate chat (delete, ban, time out) in this
  scope; that would need another ADR.

**4. OAuth scopes: one sign-in.**
- JStream signs in **once**, asking for `https://www.googleapis.com/auth/youtube`. It is
  the scope broadcast control (`liveBroadcasts.insert`, `bind`, `transition`,
  `liveStreams.insert`) and `liveChatMessages.insert` need, and the Live API reads
  accept it too (checked for `liveBroadcasts.list` and `liveStreams.list` when
  `jstream-youtube` was built). There is no second consent screen later, and no separate
  read-only grant.
- **Chat sending is a JStream setting, not an OAuth boundary.** The `youtube` scope can
  post chat. Sending stays off until the user turns it on, and the engine refuses
  `youtube.chat.send` while it is off, but the token itself could post.
- JStream asks for `youtube`, not `youtube.force-ssl`. The scopes the chat and video
  statistics reads accept are re-checked against the API reference when they are
  built; if one of them doesn't accept `youtube`, that needs a new ADR.
- Tokens stay in the OS credential store ([R-SEC-001](../RULES.md#R-SEC-001)).

**5. Quota.**
- Chat is polled at the interval the API returns, never faster.
- Stats are polled at a low fixed rate.
- The engine tracks the day's estimated quota use. When it runs low, stats polling slows
  or stops first, then chat polling. Sending and broadcast control keep working as long
  as there is quota.
- Quota errors are surfaced as protocol errors, as in ADR-0009.

**6. Timing.** Built in M3 with `jstream-youtube`; the crate itself landed early (roadmap Y1).

<a id="consequences"></a>
## Consequences
- **Unchanged from ADR-0034:** the app needs a Google Cloud OAuth client, token storage
  in the OS keychain, and a verified client for release, since `youtube` is a sensitive
  scope (otherwise users see the unverified-app warning).
- **One consent screen.** Sign-in is a single step, and broadcast control and chat work
  as soon as it finishes. `jstream-youtube`'s `OAuthClient::authorize` already takes any
  scope set, so the engine asks for `Scope::Manage` alone.
- **No read-only grant.** A user who only wants chat and stats still grants `youtube`,
  which can write. JStream's own switches (chat sending off by default, the engine
  refusing writes it isn't asked for) protect against mistakes, not against a
  compromised token. That was already true for every user of broadcast control, which
  is nearly all of them.
- **Agents can speak as the channel in public**, as in ADR-0034: the audit log and the
  "sent by an agent" marker are the safeguards.
- **Follow-on edits when this ADR is accepted** (not made while it is proposed):
  - ADR-0034: `status = "superseded"`, `superseded_by` pointing here, then
    `cargo xtask docs generate`.
  - Re-point every reference to ADR-0034 here, since references to a superseded ADR fail
    the docs check: the `Docs:` lines and READMEs of `jstream-mux`, `jstream-outputs` and
    `jstream-youtube`, `crates/jstream-protocol/README.md`, `ROADMAP.md` and the
    ARCHITECTURE crate map and integrations sections.
  - `crates/jstream-youtube/README.md`: the scopes bullet says JStream signs in once with
    `youtube`.
  - `docs/external.toml`: the `yt-live-api` entry's `relied_on` names `youtube` for every
    method this ADR lists.
