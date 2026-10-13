---
id: ADR-0009
title: "YouTube via RTMPS plus optional Live API"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: [ADR-0003, ext:yt-live-api, ext:enhanced-rtmp]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0009: YouTube via RTMPS plus optional Live API


## Context
Streaming should work with just a stream key, and agents benefit from managing the
broadcast itself (title, schedule, health, chat).

## Decision
Generic RTMP/RTMPS output (with enhanced RTMP [[ext:enhanced-rtmp]] for HEVC/AV1) works with any service.
`jstream-youtube` adds optional Google OAuth and the YouTube Live API [[ext:yt-live-api]]: create/schedule
broadcasts, bind streams, set metadata, read stream health and live chat, all exposed as
protocol commands (and so as MCP tools).

## Consequences
- Requires a Google Cloud OAuth client for the app and token storage in the OS keychain.
- API quota limits must be respected and surfaced as errors.
