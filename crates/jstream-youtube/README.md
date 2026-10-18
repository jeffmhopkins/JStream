<!-- @doc
id = "CRATE-jstream-youtube"
title = "jstream-youtube"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-youtube

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-youtube/src/**", "crates/jstream-youtube/fixtures/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-youtube", "docs/adr/0058-youtube-sign-in-with-a-jstream-client.md", "docs/RULES.md#R-SEC-001", "docs/RULES.md#R-DEP-006", "crates/jstream-platform/README.md#purpose"]
-->

Layer-2 client for Google sign-in and the
[YouTube Live API](https://developers.google.com/youtube/v3/live/getting-started), the
optional half of [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md) (the
generic RTMPS output works with a stream key alone). It has no media code and no
`unsafe`. Its JStream dependency is [`jstream-platform`](../jstream-platform/README.md#purpose),
for `Secret`. Its third-party ones are `rustls` (with `ring`) and `rustls-native-certs`
for HTTPS, `ring` for SHA-256 and the OS random source, `serde`/`serde_json`,
`thiserror` and `tracing`. It holds no state on disk and starts no threads; every call
blocks, so the engine runs it on a control or worker thread.

Contents:
- **`http`:** the `HttpTransport` trait, the crate's one seam to the network, and its
  request and response types. It exists so the API tests can swap in a fake and stay
  hermetic ([R-TEST-004](../../docs/RULES.md#R-TEST-004)); a later crate that calls
  HTTPS APIs should reuse it rather than add its own. A request's bearer token and form body are `Secret`s; a
  response body is zeroed on drop; neither type's `Debug` shows a body.
- **`HttpsTransport`:** the real transport. A small blocking HTTP/1.1 client: one
  request per connection (`Connection: close`), responses framed by `Content-Length`,
  chunked coding or the connection's end, bodies capped at `MAX_BODY_BYTES` (4 MiB),
  `https://` only. TLS is rustls with the `ring` provider and the OS trust store, as the
  RTMPS client does ([R-DEP-006](../../docs/RULES.md#R-DEP-006)); every connect, read
  and write is bounded by a timeout (`DEFAULT_TIMEOUT`, 30 s). Name resolution is left
  to the OS resolver's own limits.
- **`oauth`:** Google's
  [OAuth 2.0 for installed apps](https://developers.google.com/identity/protocols/oauth2/native-app).
  `OAuthClient::authorize` builds the consent URL with a [PKCE](https://www.rfc-editor.org/rfc/rfc7636)
  `S256` challenge (a 43-character verifier from 32 random bytes) and a random `state`;
  `exchange` trades the code and verifier for a `Grant` (access token and refresh token);
  `refresh` gets a new access token and reports a rotated refresh token; `revoke` signs
  out. Access tokens record their expiry (timed from when the request was sent) and the
  scopes granted.
  - **The built-in client** (ADR-0058 §1): `BUILT_IN_CLIENT_ID` and
    `BUILT_IN_CLIENT_SECRET` are the "Desktop app" client JStream ships. Both are empty
    until the maintainer gives their values, and while the ID is empty a build has no
    client and the engine keeps YouTube's API features off.
  - **Scopes** (ADR-0041 §4, kept by ADR-0058): `Scope::Readonly` (`youtube.readonly`) for listing and
    stream health, `Scope::Manage` (`youtube`) for creating, binding and transitioning.
    The Live API reference pages list exactly these: the list methods accept either
    (or `youtube.force-ssl`), the write methods `youtube` or `youtube.force-ssl`. A
    granted `force-ssl` counts as `Manage`. JStream signs in once, asking for
    `Manage` alone, which the reads accept too (ADR-0041 §4, kept by ADR-0058); `Readonly` stays for a
    grant that holds only it.
  - A refresh refused with `invalid_grant` is `YoutubeError::SignInRequired`.
- **`LoopbackRedirect`:** the redirect listener, bound to `127.0.0.1` on an OS-chosen
  port (`http://127.0.0.1:<port>`). Google redirects the user's browser to it, so it
  can't ask for the control token ([R-SEC-002](../../docs/RULES.md#R-SEC-002) covers
  the control listeners); instead it requires its own address in `Host` (against DNS
  rebinding) and the sign-in's `state` (compared in constant time), and the code is
  useless without the PKCE verifier, which never leaves the process. Each connection
  must send its request head within 5 s in all, and never past the wait's deadline, so a
  stalled or trickling client can't hold the wait open. Any other request gets an error
  page and the wait goes on, until the code, an `error` redirect
  (`AuthorizationDenied`) or the timeout.
- **`live`:** `LiveClient`, built from a refresh token or a fresh `Grant`. It refreshes
  the access token when less than `REFRESH_MARGIN` (60 s) of it is left, retries a call
  once with a new token after a 401, and reports a second 401 as `SignInRequired`. It
  refuses a call whose scope the grant lacks before sending it. Calls:
  `list_broadcasts` (mine, by status, or by up to 50 IDs; paged; `list_broadcasts_of_type`
  adds the `broadcastType` filter, without which the API leaves the persistent "Stream
  now" broadcast out), `broadcast`,
  `insert_broadcast`, `bind` (or unbind), `transition` (testing, live, complete),
  `list_streams`, `stream`, `insert_stream` and `stream_health` (`streamStatus`,
  `healthStatus` and its configuration issues).
  - Resources mirror the API's JSON with the fields JStream reads. Unknown fields are
    ignored, and enum values the API adds later parse as `Other`. Times stay RFC 3339
    strings.
  - `NewBroadcast::new` turns automatic start and stop off (JStream transitions the
    broadcast) and the monitor stream on (`testing` needs it). `NewStream::new` asks for
    a reusable RTMP stream with variable resolution and frame rate.
  - A stream's key (`cdn.ingestionInfo.streamName`) parses straight into a `Secret`.
- **`chat`:** reading a broadcast's live chat (ADR-0041 §2, §5).
  `LiveClient::chat_messages(live_chat_id, page_token)` reads one page of
  `liveChatMessages.list` (`part=id,snippet,authorDetails`, up to 2000 messages, oldest
  first; `Scope::Readonly`, which the method accepts along with `youtube`). A
  `ChatMessage` keeps its ID, time, author (name, channel, and whether owner,
  moderator, member or verified), the text YouTube displays for it, and its event:
  text, Super Chat or Super Sticker (amount as displayed, currency, micros),
  membership, a deletion (naming the removed message), or `Other` by its `type`. Text
  and names come from strangers: control characters are replaced and they are cut to
  1000 and 100 characters, and nothing here interprets them. `ChatPoller` drives one
  chat: it follows the page token, waits the `pollingIntervalMillis` the API returns
  (never faster, and at least 1 s), backs off doubling after errors (from 60 s up to
  30 min for quota and rate-limit errors, from 5 s up to 5 min for others; a change of
  class starts that ladder at its first step), and stops
  for good when a page reports `offlineAt` or the API answers `liveChatEnded`,
  `liveChatNotFound` or `liveChatDisabled`. It takes the time as an argument, starts
  no thread and never sleeps: the engine's loop calls `poll` and waits until `due`.
  Every page costs quota from the one daily quota JStream's built-in client shares
  across its users, so reading chat through the API is opt-in: a per-collection
  switch, off by default, decides whether the engine polls at all (ADR-0058 §5). The
  "YouTube chat" browser preset shows chat at no quota cost.
- **`stats`:** audience stats (ADR-0041 §3, kept by ADR-0058). `LiveClient::audience_stats`
  reads a broadcast's concurrent viewers, likes and views, and its actual start and end
  times, with one
  [`videos.list`](https://developers.google.com/youtube/v3/docs/videos/list) call
  (`part=id,liveStreamingDetails,statistics`, one unit of quota) under
  `Scope::Readonly`, which the one `youtube` sign-in covers. A count YouTube leaves out
  is `None`, never 0: the viewer count while nobody watches, once the broadcast ends,
  or when the owner hides it, and a hidden like count. `STATS_INTERVAL` (60 s) is the
  low fixed rate the engine polls at (ADR-0041 §5); this crate keeps no timer.
  `ChatRate` turns the message count of each chat poll into messages per minute over
  the last minute (`CHAT_RATE_WINDOW`): the messages of the polls after the oldest one
  in the window, over the time from it to the newest, since a poll returns what arrived
  since the one before (so a first poll's backlog is not a burst). It is `None` until
  two polls are in the window, and keeps at most 128 counts, never a message.
- **`fake`** (feature `fake`, and in this crate's tests): `FakeTransport` replays
  queued responses and records requests. Tests run on JSON fixtures in `fixtures/`,
  shaped after the API reference's documented responses, so no test uses the network
  ([R-TEST-004](../../docs/RULES.md#R-TEST-004)); the HTTPS tests use a TLS server on
  loopback with certificates made at test time.
- **Errors:** one `thiserror` enum, `YoutubeError`. `is_quota` recognises quota and
  rate-limit reasons, which the engine surfaces as protocol errors (ADR-0041 §5, kept by ADR-0058).

**Secrets** ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): tokens, codes, the client
secret and stream keys are held as `Secret`s. The request head that carries the bearer
token, and every response body, are zeroed after use; like `Secret`'s own wiping, that
is best effort, and buffers inside rustls are not reached. No error, log line or `Debug` output carries one: errors from Google
keep only its error code and message (cut to `MAX_REMOTE_TEXT` characters, control
characters replaced), and a response that fails to parse is reported by line and
column, never quoted. The refresh token is kept in the OS credential store through
`jstream-platform`'s `OAuthTokenStore`, by the host: this crate hands it over in a
`Grant` and through `LiveClient::take_rotated_refresh_token`, and never writes it
anywhere itself.

The engine (`jstream-core`, roadmap Y3) holds one `LiveClient`, keeps its refresh token
in the `OAuthTokenStore` and exposes these calls as protocol commands for the UI. MCP
leaves them out: YouTube is for people, not agents (ADR-0058 §3).

**Not yet:** sending chat as the channel, the engine's chat polling with its
per-collection opt-in switch and its `youtube.chat.feature` action,
`liveChatMessages.streamList` where the API offers it (ADR-0058 §5), the engine's
stats polling and its `youtube.stats` event and query (protocol work), and quota
accounting.
