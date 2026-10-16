<!-- @doc
id = "ADR-0051"
title = "YouTube v4: no required OAuth client"
kind = "adr"
status = "superseded"
tier = "maintainer"
superseded_by = "docs/adr/0058-youtube-sign-in-with-a-jstream-client.md"
-->

# ADR-0051: YouTube v4: no required OAuth client

<!-- @generated begin adr-status -->
> **Superseded** by [ADR-0058: YouTube sign-in with JStream's own OAuth client, for people only](0058-youtube-sign-in-with-a-jstream-client.md). This decision no longer applies.
<!-- @generated end adr-status -->

Supersedes [ADR-0041](0041-youtube-interaction-scope-v3.md). It restates
ADR-0041's decision, which stands, except for one change: the YouTube Live API features
become optional. They turn on only when a user configures **their own** Google OAuth
client. JStream never embeds one.

<a id="context"></a>
## Context
ADR-0041 (like ADR-0009 and ADR-0034 before it) has JStream sign in to Google with
"the app's OAuth client". Its consequences already note the cost: a Google Cloud OAuth
client, and Google's verification before release, because `youtube` is a sensitive
scope. Until then users see the unverified-app warning.

The maintainer wants to avoid that dependency: "I think the oauth client id is
something we should look to avoid requiring". Running an OAuth client means a Google
Cloud project, its quota, a verification review and keeping its credentials in every
build. Product scope is a maintainer decision
([ADR-0043](0043-narrow-maintainer-tier.md)).

Most of what a streamer needs from YouTube works without the API:
- **Going live:** a stream key over RTMPS already works (M1). With auto-start on in
  YouTube Studio, the broadcast goes live when data arrives, and with auto-stop on it
  ends when the data stops.
- **Seeing chat:** YouTube's public popout chat page, which shows a live video's chat,
  needs no sign-in, so a browser source ([ADR-0021](0021-browser-source.md)) can show
  it.

<a id="decision"></a>
## Decision

**1. The default is no OAuth at all.**
- A `youtube` stream goes out as a generic RTMPS output with the user's stream key
  (ADR-0009). That needs no sign-in, no Google client and no API quota.
- **Chat without OAuth:** a "YouTube chat" preset of the browser source. It is a
  browser source with the popout chat page for a live video. Its main setting is the
  user's channel (a handle such as `@name`, or a channel URL), so it is set up once:
  each time it loads, and while it shows no chat, it opens the channel's public live
  page to find the current live video and shows that video's chat, with no API call.
  A video ID or link can be given instead, to pin one broadcast. It is read-only: it
  shows chat, and it can't send, feature or moderate. The pages are YouTube's own, so
  they run under ADR-0021's browser rules like any other page.

**2. The Live API is optional: the user brings their own client.**
- A user who wants more enters **their own** Google OAuth client ID (and its secret, if
  the client type has one) in JStream's settings, with no file to edit. The client is
  set up in their own Google Cloud project, under their own quota. The ID and secret
  are kept in the OS credential store, like the refresh token: never in a collection
  file, a protocol message or a log, and the secret is never shown back.
- **JStream never embeds an OAuth client** in any build, and never ships a default one.
- With a client configured, everything ADR-0041 describes turns on for that user's
  account, unchanged:
  - the one sign-in with the `youtube` scope (§4);
  - creating and scheduling broadcasts, and setting title, description and privacy;
  - binding streams, explicit go-live and end, and stream health;
  - reading chat through the API, the featured message and its native chat-message
    source (§2), and sending chat as the channel (§1);
  - audience stats (§3) and quota handling (§5).
- **Without a client, those features are hidden, not errors.** `youtube.status`
  reports `unavailable`, the UI hides YouTube's sign-in and broadcast controls, and
  the MCP server leaves the `youtube` tool out of its tool list. The protocol still
  defines the YouTube calls, so a client that sends one anyway gets `action_failed`
  saying no client is configured. A collection holding an output bound to a YouTube
  broadcast still loads; that output fails to start, saying the same.

**3. Everything else in ADR-0041 stands** for a user who configured a client: the
scope, chat sending off by default, the audit log, featured-message state, and quota
behaviour.

<a id="consequences"></a>
## Consequences
- **No Google dependency for the project.** There is no OAuth client to register,
  verify, keep under quota or rotate, and no client credential in any build. ADR-0041's
  consequence about a verified client for release no longer applies.
- **Without a client, users lose:** setting the title, description, thumbnail and
  privacy from JStream; creating and scheduling broadcasts; explicit go-live and end
  (auto-start covers the common case); stream health and viewer counts; sending chat;
  featuring a message; and moderation (which ADR-0041 already left out of scope).
- **With their own client, users take on Google's setup:** creating a Cloud project,
  enabling the YouTube Data API, making an OAuth client for a desktop app, and
  accepting the unverified-app warning for their own unpublished client. The docs need
  a how-to for this.
- **Chat without a client is display-only,** and as reliable as YouTube's popout and
  live pages: if YouTube changes or blocks them, the preset breaks while the API path
  keeps working. Following a channel finds a stream once it is live; a scheduled
  broadcast that hasn't started shows no chat until it does.
  The page's look comes from YouTube, styled only as far as browser-source CSS allows.
- **Code that follows (on acceptance):**
  - Y3 (#223) and A6b (#312) treat YouTube as "off unless the user supplied a client":
    the engine gets a YouTube setup only from user configuration, and the wording of
    `unavailable` and its errors says to configure a client.
  - The MCP server lists the `youtube` tool only when the host says a client is
    configured.
  - Y4 becomes the "YouTube chat" browser-source preset (after C8). The API chat
    reader and the chat-message source stay as part of the optional path.
- **Follow-on doc edits (on acceptance, not while proposed):**
  - ADR-0041: `status = "superseded"` and `superseded_by` pointing here, then
    `cargo xtask docs generate`.
  - Re-point references to ADR-0041 here, since references to a superseded ADR fail
    the docs check: the `Docs:` lines and READMEs of the crates that cite it, the
    ARCHITECTURE crate map and protocol sections, and `docs/external.toml`'s
    `yt-live-api` entry.
  - The roadmap's Lane Y and the go-live runbook describe the no-OAuth default first.
