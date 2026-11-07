<!-- @doc
id = "ADR-0124"
title = "What a page may read: one checkbox and one warning per kind of data"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0124: What a page may read: one checkbox and one warning per kind of data

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0102](0102-page-api-for-browser-sources.md) lets a `browser` source opt its page in
to a read-only `window.jstream`. All of it is built: the clock and the page's own
visibility, the scenes, shared values, and one bus's levels and bands (#1127, #1146,
#1263). #1228 asks for a review of every member against what a page needs, with a
per-source checkbox for each kind of data, so that no page gets data it shouldn't.

Today the grant is four settings in the Properties dialog's **Page API** group.
`page_api` turns the API on, and `page_api_scenes` is a checkbox. Shared values are a
names field, where empty grants none. A bus is a bus picker, where none grants none.
Each setting's warning is its `description`, which the dialog shows only as a tooltip.
Nothing in the source list says a page is granted anything.

A page served from the web, or one that loads web content, can send whatever it reads off
the machine. That makes the shape of the grant part of JStream's privacy posture.

**Tier.** This ADR is maintainer tier under [ADR-0043](0043-narrow-maintainer-tier.md),
for the same reason ADR-0102 is: it decides what engine data an untrusted page may
receive. It amends ADR-0102 decision 1 (the settings and what the dialog says). It also
records the review #1228 asks for, which this ADR's own decisions follow from.

<a id="decision"></a>
## Decision
1. **The review.** Each member of `window.jstream`, what it reveals, and how it is
   granted:

   | Member | What it reveals | Granted by | Warning shown beside it |
   |---|---|---|---|
   | `clock` | The engine's animation clock: how long the engine has run, nothing about the show | `page_api` | (the switch's own, below) |
   | `source` (`shown`, `previewShown`, their times, `shown` and `hidden` events) | When this source is on program or preview | `page_api` | (the switch's own, below) |
   | `scenes` | The program and preview scenes' names | `page_api_scenes` | "Preview scene names can say what hasn't been shown yet." |
   | `values` | The named shared values the grant covers | `page_api_shared_values`, then `page_api_values` | "The page can read these values, and a page from the web can send them anywhere." |
   | `levels()` and the `levels` event | One bus's post-fader peak and RMS, and 64 bands | `page_api_audio`, then `page_api_bus` | "A page given a bus can follow the shape of its sound, and a page from the web can send that anywhere. Give it only the bus it shows." |

   The clock and the page's own visibility stay with `page_api` itself. They say only
   when this source is on air, which the page can largely see from its own paints
   anyway, and every granted page needs them to animate in step. The switch's warning is:
   "A page from the web can send what it reads anywhere."
2. **One checkbox per kind of data.** Each kind of data beyond the clock and visibility
   is a checkbox in the Page API group. Each checkbox is off by default and shown only
   while `page_api` is on:
   - **Scenes:** `page_api_scenes`, unchanged.
   - **Shared values:** a new `page_api_shared_values` (`bool`). `page_api_values`, the
     names, is shown only while it is on, and is read only while it is on.
   - **Audio levels:** a new `page_api_audio` (`bool`). `page_api_bus` is shown only
     while it is on, and is read only while it is on.

   Granting a microphone bus thus takes a checkbox and a bus, never a bus picked in
   passing.
3. **Existing grants carry over.** A collection saved before this change has no
   `page_api_shared_values` or `page_api_audio`. Loading it rebuilds each source with its
   kind's defaults, which would turn both off, so the collection file's schema goes to
   version 8. Its migration turns each checkbox on in a `browser` source whose names or
   bus are set. Nothing a user granted is silently taken away, and nothing new is
   granted.
4. **The warning is visible.** Protocol 0.95 adds `warning` to a parameter's descriptor:
   one line that a client shows beside the control, not only in a tooltip. Each setting
   in the table above carries its warning there. A client that predates `warning`
   ignores it.
5. **A badge while anything is granted.** The Sources panel shows a small badge on the
   row of each `browser` source with `page_api` on. Its tooltip names what is granted,
   for example "Page API: clock, visibility, scenes, audio levels (Mic)".
6. **Never exposed.** No setting grants a page any of these, and later members must not
   either: raw audio samples; another source's pictures or content; the control API or
   any command; the control token, other credentials, or file paths outside the page's
   own folder; the output's targets or stream keys. ADR-0021's control opt-in stays
   unbuilt (#347).
7. **Later kinds of data get the same treatment.** Tempo and MIDI (#1222), and any later
   member, each get their own checkbox and visible warning in this group, off by
   default, before any page can read them. MIDI gets the warning "A page given MIDI sees
   every note and control on its port."
8. **Guidance.** The MCP server instructions and the `jstream-sources` docs show how to
   build a spectrogram page on `jstream.levels()`: grant the bus, draw each `levels`
   reading as a column, and fall back to blank on `null`.

<a id="consequences"></a>
## Consequences
- A user sees, per source, which kinds of data it is granted: in the Properties dialog,
  each with its warning in plain sight, and in the Sources panel's badge.
- Two settings are added to the `browser` kind, and protocol 0.95 adds `warning` to
  parameter descriptors. The `kinds.snap` diff reviews both.
- Grants made before this change keep working (decision 3). The schema-8 migration
  follows lane D's version 7 (#1270).
- The threat model's browser section records these controls.
- Tempo and MIDI can't reach a page by being added to `window.jstream` alone: each
  needs its own checkbox first (decision 7).
