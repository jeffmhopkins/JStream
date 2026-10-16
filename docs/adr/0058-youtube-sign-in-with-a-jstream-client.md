<!-- @doc
id = "ADR-0058"
title = "YouTube sign-in with JStream's own OAuth client, for people only"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0058: YouTube sign-in with JStream's own OAuth client, for people only

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

Supersedes [ADR-0051](0051-youtube-without-a-required-oauth-client.md) §2 ("the user
brings their own client"), and makes the YouTube API features a person's tools only,
an exception to [ADR-0006](0006-mcp-full-access.md). The rest of ADR-0051 stands: going
live with a stream key and the "YouTube chat" browser-source preset still need no
sign-in at all.

<a id="context"></a>
## Context
ADR-0051 made the YouTube Live API optional and put it behind a Google OAuth client that
each user creates in their own Google Cloud project. The first real test of that path
(the Windows run in issue #337) asks the user to create a Cloud project, enable the
YouTube Data API, make a "Desktop app" client and download its JSON. The maintainer
rejected that: "I don't want cloud console stuff. That's against intent, is not ease for
users." The maintainer also decided that agents should not control YouTube: "I don't think agents
should control YouTube at all. That is ok being person only if it prevents cloud console
stuff."

The YouTube Data API has no anonymous access for a channel's own broadcasts and chat:
every call that touches them needs an OAuth token issued to *some* registered client. So
"no user setup" means the project registers that client once, for everyone. The choices:

- **A JStream client shipped with the app.** Google's
  [guide for installed apps](https://developers.google.com/identity/protocols/oauth2/native-app)
  says installed apps "cannot keep secrets", makes the client secret optional in the code
  exchange, and recommends PKCE (`S256`) with a loopback redirect. That is the flow
  `jstream-youtube` already implements. The user clicks sign in, approves in their browser,
  and is done.
- **The device flow** ([TVs and limited-input devices](https://developers.google.com/identity/protocols/oauth2/limited-input-device)):
  the app shows a code and the user enters it on any device. It allows the `youtube` and
  `youtube.readonly` scopes, but Google points apps that have a browser to the loopback
  flow instead, and it needs a second client of another type. It is still a JStream-owned
  client, so it removes no setup.
- **A token broker** (a JStream web service that holds a confidential client and hands
  out tokens): it costs hosting, puts every user's YouTube token through a server the
  project runs, and fails when that server is down. Nothing about Google's rules for
  installed apps needs it.

What Google asks of a shared client:
- **Audience and verification** ([Manage app audience](https://support.google.com/cloud/answer/15549945)):
  while the consent screen is in "Testing", only listed test users (up to 100) can sign
  in, and their refresh tokens expire seven days after consent. In production but
  unverified, users see the unverified-app screen, and the project may take only 100 new
  users in total, a cap that never resets.
- **Verification for sensitive scopes**
  ([brand verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/brand-verification)):
  a homepage and privacy policy on a domain the project owns and has verified in Search
  Console, a demo video of the sign-in, and a justification for each scope. The annual
  security assessment applies to restricted scopes only. `youtube` is listed as a
  sensitive scope; the consent screen's scope picker shows the class when the client is
  set up.
- **Quota** ([quota and compliance audits](https://developers.google.com/youtube/v3/guides/quota_and_compliance_audits)):
  a project gets 10,000 units a day, shared by everyone who uses its client, reset at
  midnight Pacific time. More needs a compliance audit against the YouTube API Services
  terms. By the [cost table](https://developers.google.com/youtube/v3/determine_quota_cost),
  broadcast control is cheap (1 unit for `liveBroadcasts.insert` or `transition`) but every
  `liveChatMessages.list` call costs 5, so API chat reading is what spends a shared quota.

<a id="decision"></a>
## Decision

**1. JStream ships one OAuth client.**
- It is a Google OAuth client of type "Desktop app", in a Google Cloud project owned by
  the maintainer. Its ID and secret are built into JStream as constants in
  `jstream-youtube`; Google treats an installed app's secret as non-confidential, so it is
  not a credential to protect, though it is still held as a `Secret` so it never prints.
- Sign-in stays as `jstream-youtube` does it: PKCE `S256`, a loopback redirect, one
  consent for the `youtube` scope ([ADR-0041](0041-youtube-interaction-scope-v3.md) §4).
  Tokens stay in the OS credential store.
- **Users set nothing up.** YouTube's sign-in shows in the UI in every build that has the
  client. Until the constants are filled in, builds behave as ADR-0051 describes for "no
  client": YouTube's API features are hidden.

**2. The Google project is the maintainer's, not an agent's.**
- Creating the project and client, the consent screen, verification, the quota audit and
  any rotation are the maintainer's account actions. No agent holds access to that
  project.
- Changing the built-in client ID or secret is a maintainer decision: an agent commits
  only values the maintainer gave it.
- Granting access is always the account holder's own step in their browser.

**3. YouTube is for people, not agents.**
- The MCP server lists no `youtube` tool and forwards no `youtube.*` request: signing in
  and out, broadcasts, binding, transitions, chat reading and sending, the featured
  message and stats are all out of reach of agents. ADR-0041 §1's "agents may send" chat
  no longer holds.
- The protocol keeps the `youtube.*` calls for the UI, which is the only client JStream
  ships that sends them. An agent holding the control token could still speak the raw
  protocol; JStream can't tell that apart from the UI, so the guarantee covers JStream's
  agent interface, MCP.
- Not affected: a plain RTMPS output with a stream key is a generic output, not the
  YouTube API, so agents can still configure, start and stop it, as for any service.
  Events the UI shows, such as YouTube stream health, are not sent to MCP.

**4. No user-supplied client in the product.**
- ADR-0051 §2's settings field for the user's own client is dropped. The daemon's
  `--youtube-client` flag and the `JSTREAM_YOUTUBE_CLIENT_ID` and
  `JSTREAM_YOUTUBE_CLIENT_SECRET` variables stay as a developer override, for forks and
  for testing another client. User docs don't describe them.

**5. Quota is shared, so API chat reading is opt-in.**
- The "YouTube chat" browser-source preset (ADR-0051 §1) stays the default way to show
  chat, at no quota cost.
- Reading chat through the API, which the featured message needs, is a per-collection
  switch, off by default. When on, it uses `liveChatMessages.streamList` where the API
  offers it, and otherwise polls at the interval the API returns.
- ADR-0041 §5's quota handling stands. A quota error says the day's shared quota is used
  up and when it resets.

**6. Before a public release**, the maintainer publishes the consent screen to production
and gets the client verified, and files the YouTube API audit for more quota. Until then
the client stays in "Testing", for listed test users only.

<a id="consequences"></a>
## Consequences
- **Signing in to YouTube is one click** for users, with no Google Cloud steps. Going live
  with a stream key and showing chat with the preset still need no sign-in.
- **The maintainer takes on a Google project:** one-time setup now, then verification and
  the quota audit before release. Verification needs a homepage and privacy policy on a
  domain the project owns: a `github.io` address can't be verified, so a domain is a
  small yearly cost. The privacy policy can be short, because JStream sends YouTube
  tokens and data only between the user's machine and Google.
- **Until verification**, the client works only for listed test users, and their sign-in
  lasts seven days, so a tester signs in again each week. That is enough for hardware
  runs like #337.
- **Agents lose YouTube.** No agent can create or end a broadcast, set its metadata, read
  chat through the API, send chat or feature a message; a person does those in the UI.
  The MCP tool list and the chat panel's "sent by an agent" marker for YouTube go away.
- **Every user shares one daily quota.** Broadcast control costs little; API chat
  reading is opt-in and is what the quota audit must cover. If the quota runs out, the
  stream itself keeps going: only API calls fail, as ADR-0041 §5 already handles.
- **Forks and source builds carry the client too.** That is how every desktop OAuth app
  works, and Google expects it of installed apps. A fork that wants its own quota uses the
  developer override.
- **Not chosen, can be added later as agent-tier work:** the device flow, for a daemon on
  a machine with no browser. It would be a second client in the same project, again with
  no user setup.
- **Code that follows (on acceptance):**
  - `jstream-youtube` gets the built-in client constants (empty until the maintainer gives
    the values), and the daemon and engine use them when no override is given (A6b, #312;
    Y3, #223). `youtube.status` reports `unavailable` only in builds without them.
  - The MCP server drops its `youtube` tool and YouTube events (A6b, #312; Y3, #223).
  - The API chat reader gets its opt-in switch (Y4 and the chat work after it).
  - Issue #337's steps lose the client file: run, `youtube.sign_in`, approve.
- **Follow-on doc edits (on acceptance, not while proposed):**
  - ADR-0051: `status = "superseded"` and `superseded_by` pointing here, then
    `cargo xtask docs generate`.
  - Re-point references to ADR-0051 here: the `Docs:` lines and READMEs of the crates
    that cite it, ARCHITECTURE, `ROADMAP.md`, the go-live runbook and
    `docs/external.toml`'s `yt-live-api` entry. The go-live runbook's "bring your own
    client" section becomes "sign in".
