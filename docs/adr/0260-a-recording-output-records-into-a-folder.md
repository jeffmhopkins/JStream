<!-- @doc
id = "ADR-0260"
title = "A recording output records into a folder"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0260: A recording output records into a folder

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context

A `record` output names one file. A recording never overwrites, so once that file
exists the next `output.start` fails, and the person has to pick a new path before
each take (#1072 added `output.set_path` for that). A collection also starts with no
recording output, so Record is disabled until one is set up, and a recording output
with no encoder profile always takes the engine's default, so settings chosen for one
collection's recordings are gone in the next (#1823).

The protocol only grows by additions ([R-API-001](../RULES.md#R-API-001)), so
`path` can't change its meaning for clients that send a file there.

<a id="decision"></a>
## Decision

1. **A `record` output can name a folder.** `OutputKind::Record` gains `folder`
   (protocol 0.199). With it, `path` is a folder, and each `output.start` records
   to a new file there: `JStream_<date>_<time>.<ext>`, such as
   `JStream_2026-11-28_10-17-30.mp4`, stamped with the engine machine's local time
   when it starts. When that name or its `.part` name is taken, `_2`, `_3`, … go
   before the extension, so two starts in the same second never overwrite each other.
   The prefix and the stamp's format are fixed for now. Without `folder` an output
   still names one file, as before, so an older client's `output.create` keeps its
   meaning.
2. **A missing folder is made.** The start makes the folder and any missing parent.
   When it can't, or can't write the file there, the start fails with
   `action_failed` naming the folder, and nothing starts. The folder's path is checked
   like a file's (absolute, no UNC or device path, no `..`), but may end in a
   separator or be a drive's root.
3. **An empty folder is the engine's own.** It is `JStream` in the person's Videos
   folder (`Movies` on macOS, `XDG_VIDEOS_DIR` on Linux), which the host names
   (`Engine::with_recordings_folder`). A collection stays portable: the same output
   records into each machine's own folder. An engine with no folder of its own fails
   the start with `action_failed`.
4. **A collection starts with a recording output.** A new collection gets
   `recording`, a folder output with an empty folder, so Record works before anything
   is set up. It is an ordinary output, which the person can change or delete.
5. **Older collection files move to folders.** Collection file version 24 turns each
   version 23 recording output into a folder output into its file's folder (the file
   name is dropped), and adds `recording` to a file that has no recording output. The
   migration runs once, so a deleted `recording` stays deleted.
6. **The last recording's encoding carries over, per machine.** Every recording that
   starts saves its encoding in `last-recording.json` in the data folder. A recording
   output with no encoder profile of its own records with that encoding, or with the
   engine's default before the first recording. It is per machine rather than per
   collection, so a recording in any collection opened next takes the settings last
   recorded with.

<a id="consequences"></a>
## Consequences

- Pressing Record twice makes two files and never fails on a taken name.
- A recording with no profile of its own can encode differently from a stream with
  none, so the two may no longer share an encoder session after a recording on
  another profile ([ADR-0080](0080-program-master-and-output-chains-v2.md)). Give both
  the same profile to share one.
- A client older than 0.199 reads a folder output as a file output with that path.
  Its `output.set_path` on one is checked by the folder rules.
- The file name pattern is not configurable. A prefix or format setting can be added
  later as another optional field.
- `output.status` does not name the file a running folder recording writes yet. The
  engine logs it when the recording starts.
