# 0009: YouTube via RTMPS plus optional Live API

- Status: Accepted
- Date: 2026-10-13

## Context
Streaming should work with just a stream key, and agents benefit from managing the
broadcast itself (title, schedule, health, chat).

## Decision
Generic RTMP/RTMPS output (with enhanced RTMP for HEVC/AV1) works with any service.
`jstream-youtube` adds optional Google OAuth and the YouTube Live API: create/schedule
broadcasts, bind streams, set metadata, read stream health and live chat, all exposed as
protocol commands (and so as MCP tools).

## Consequences
- Requires a Google Cloud OAuth client for the app and token storage in the OS keychain.
- API quota limits must be respected and surfaced as errors.
