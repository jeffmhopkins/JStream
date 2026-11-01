<!-- @doc
id = "ADR-0083"
title = "The activity log over the protocol"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0083: The activity log over the protocol

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The prototype's Agent activity panel lists what agents changed, newest first: a meta line
"MCP · 2 min ago" over a sentence such as "Switched to “Desktop + Cam”" or "Mic gain −1.5 →
−3.0 dB" (gap row AG-01, [UI-FEATURES](../UI-FEATURES.md#live-panels)). Decision
register D38 ([design/ui-prototype](../../design/ui-prototype/decision-register.md))
names the gap: the engine already records every batch, undo, redo and action in its audit
log ([ADR-0006](0006-mcp-full-access.md), [ADR-0013](0013-command-model.md),
[ADR-0032](0032-live-edits-and-undo.md)), but only into a file on the host, and
[R-ARCH-001](../RULES.md#R-ARCH-001) requires the UI to read what it shows over the
protocol. D38 sketched `audit.list` and `audit.appended` with summaries the engine
writes. Event subscriptions don't exist yet (protocol open question 7), so an event is
sent to every client, MCP agents included, and every engine and server test that counts
the events a call sends would count one more.

The audit file deliberately carries names, IDs, revisions and error codes only, never a
call's arguments, so no stream key, URL or setting can reach it
([R-SEC-001](../RULES.md#R-SEC-001)). It records only when the host gave it a sink. A
sentence like "Mic gain −1.5 → −3.0 dB" needs a name and two values, and the old value is
gone once the batch applies; a deleted scene's name is gone too.

<a id="decision"></a>
## Decision
1. **The engine keeps the recent activity in memory.** Every call the audit log records
   (a batch, an undo or redo, an action other than `live.set` and `live.cancel`, and a
   gesture that ended without a commit) also becomes an *activity entry* in a list of
   the last 100, numbered by a `seq` that starts at 1 and grows by one per entry. It is
   kept whether or not the host gave an audit sink, and lost when the engine stops: the
   audit file stays the durable record.
2. **Each entry carries one English sentence the engine writes**, from the call and the
   collection as it stood just before the call ran, so a deleted object keeps its name
   and a setter can say what it changed from. Gain, mute, opacity and visibility setters
   give before and after values ("Mic gain −1.5 → −3.0 dB"); other edits name their verb
   and target ("Renamed scene “Intro”"); a batch of several edits is led by its delete,
   if it has one, else by its first edit, with "and N more changes". The sentence names
   objects and states numeric, on/off and enum values only. It never holds a source's
   settings, an output's target, a stream key, a filter's settings or any other
   free-form argument, so R-SEC-001 holds without a redaction list.
3. **The audit file is unchanged.** The sentence is not written to it; its lines keep
   their argument-free shape.
4. **Protocol 0.51 adds** the `activity.list` query, which answers the newest entries
   first (at most `limit`, 100 by default and at most, and only those after `after_seq`
   when given). **No event** announces a new entry until event subscriptions exist: a
   client that shows the log asks again with `after_seq`, which costs one small answer.
   An `activity.added` event can join with subscriptions in a later minor. The answer
   carries the engine's clock (`now_ms`), so a client on another machine can say how long
   ago an entry was without trusting its own clock. An entry has
   `seq`, `time_ms` (Unix milliseconds), `client_id`, `client_name`, `call` (the audit
   log's call name), `summary`, and `error` (the protocol error code) when the call
   failed. WASM plugins can't call `activity.list` whatever they are granted
   ([ADR-0069](0069-wasm-plugin-limits-and-capabilities.md)): what other clients did,
   and their names, are the user's to read.
5. **The UI's Agent activity panel shows what agents did.** It lists entries from every
   client that is not a JStream UI (its `client_name` is not `jstream-ui` and does not
   start with `device:`, the name a paired browser's calls carry,
   [ADR-0044](0044-pairing-remote-browsers.md)), newest first. The meta line is the
   client's name and the time since, in the prototype's mono style; a failed call adds
   its error code. While it is shown it reads `activity.list` when it opens, again with
   `after_seq` after each `state.changed`, `studio.changed` and `output.state_changed`
   (which most entries follow), and every 5 seconds, which catches failed calls and
   actions that send no event.

<a id="consequences"></a>
## Consequences
- An agent can also read what other agents and the user's UI did, with `activity.list`,
  and every client can show the panel. Every authenticated client already has full
  access (ADR-0006), so nothing new is exposed beyond names and the values `state.get`
  already returns.
- The engine owns one table of sentences, which a new edit or action has to join; one
  it doesn't know falls back to its command name, so a missing sentence is ugly, never
  wrong.
- The sentences are English. A localised UI would need a structured form next to them,
  which a later minor can add.
- The panel can lag a failed call or a quiet action by up to 5 seconds, and asks the
  engine every 5 seconds while it is shown.
- The list lives in memory, so a restarted engine starts empty, and an entry older than
  the last 100 can only be read from the audit file on the host.
