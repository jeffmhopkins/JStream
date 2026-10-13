+++
id = "ADR-0009"
title = "YouTube via RTMPS plus optional Live API"
kind = "adr"
status = "accepted"
+++

# ADR-0009: YouTube via RTMPS plus optional Live API

<a id="context"></a>
## Context
Streaming should work with just a stream key. Agents also benefit from managing the
broadcast itself: title, schedule, health and chat.

<a id="decision"></a>
## Decision
A generic RTMP/RTMPS output (with enhanced RTMP for HEVC/AV1) works with any service, and
reconnects with backoff. `jstream-youtube` adds optional Google OAuth and the
[YouTube Live API](https://developers.google.com/youtube/v3/live/getting-started):
creating and scheduling broadcasts, binding streams, setting metadata, and reading stream
health and live chat. These are exposed as protocol edits, actions and queries (and so
over MCP). An `Rtmp` output may be bound to a YouTube broadcast.

<a id="consequences"></a>
## Consequences
- Requires a Google Cloud OAuth client for the app, and token storage in the OS keychain.
- API quota errors are surfaced as protocol errors.
