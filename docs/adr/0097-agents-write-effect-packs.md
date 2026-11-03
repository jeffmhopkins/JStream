<!-- @doc
id = "ADR-0097"
title = "Agents write effect packs over the protocol and MCP"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0097: Agents write effect packs over the protocol and MCP

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0093](0093-shader-sources-from-effect-packs.md) lets an effect pack draw shader
sources: warp fields, tunnels, reflections and audio-driven looks that agents building
scenes over MCP want. A pack is still a folder a person puts in the packs folder. No
command writes one, so an agent can't add a shader, see why it fails to compile, or fix
it, and the scene-building loop (write, check a snapshot, adjust) stops at the shader.

Letting a client write packs changes what a remote client is trusted to do, which
[ADR-0043](0043-narrow-maintainer-tier.md) makes a maintainer decision:
- Installing extension code has been a person's act. [ADR-0069](0069-wasm-plugin-limits-and-capabilities.md)
  keeps WASM plugin installs off MCP, and [ADR-0087](0087-effect-quarantine-after-gpu-device-loss.md)
  keeps `effect.enable` off MCP so "an agent can't put a shader that hung the GPU back on
  air". `jstream-mcp` enforces both with its `PEOPLE_ONLY` nouns.
- The threat model treats a pack as GPU code the user installed, and the packs folder as
  trusted as the rest of the data folder. naga validates WGSL, but nothing bounds how long
  a shader runs (#349).
- ADR-0087 keys its quarantine by kind ID. A client that writes packs chooses the kind ID,
  so it could put a blamed shader back under a new one.

<a id="options"></a>
## Options
- **A. Agents write packs (recommended).** The decision below. An agent can make and fix
  shaders in the scene-building loop; the cost is that a token-holding client can put
  GPU code on the machine without a person installing it, and can replace or delete any
  pack a person installed, by that pack's own ID.
- **B. Agents write packs only with a person's approval.** A write would wait in a
  queue until a person accepts it in the UI. It keeps installs a person's act, but the
  loop stalls on every compile error, and the approval UI and queue are new work of their own.
- **C. Packs stay a person's files.** Nothing changes. Shader sources still work for packs
  a person installs, and an agent can only describe a shader for a person to save.

<a id="decision"></a>
## Decision
1. **Agents may write packs.** The protocol gains `pack.write` and `pack.delete` actions
   and `pack.list` and `pack.read` queries, and `pack` is not a `PEOPLE_ONLY` noun, so
   MCP agents get them. For WASM plugins (amends ADR-0069 S2's table), `pack.list` and
   `pack.read` are in the `read` class as every query is, and `pack.write` and
   `pack.delete` are "never".
   - `pack.write {pack, files}` replaces one pack whole with the files it carries (text
     as text, images as base64) and loads it at once. It answers with the pack's kinds,
     or with `invalid_argument` naming each problem's file, line and message, and then
     the old version stays.
   - `pack.delete {pack}` removes the folder whose `effect.toml` names pack ID `<pack>`,
     whatever its name and whether or not it loads, or is refused with `not_found` when
     none does; `pack.list` names the packs,
     their folders, their kinds and who last wrote each (point 5).
   - `pack.read {pack}` returns one pack's text files and, for each image, its path,
     size and digest; `pack.read {pack, path}` returns one file, an image as base64. An
     image over 2 MiB, which only a person can have installed (point 3), is refused
     with `too_large`, so no answer holds more than about 2.7 MiB of base64.
   - These are actions, not edits: undo doesn't reach the disk.
2. **Where a write lands.** `pack` is a pack ID (ADR-0093's form: lower-case letters,
   digits and `-`), which can't name a dot-file, so the pack is always the folder
   `<packs>/<pack>` and nothing else: never `quarantine.json`, `.staging`, `.trash` or
   `.deleted`.
   Its `effect.toml` must have `id = <pack>`. Each file path is relative and follows the
   asset-path rules, applied to each component as the platform parses it: no `..`, no
   absolute, drive or UNC path, no dot-component, no `\` or `:`, no Windows reserved
   name (`CON`, `NUL`, `COM1` and the like, with any extension: `nul.png` too) and no
   trailing dot or space; and it names
   `effect.toml` or a `.wgsl`, `.png`, `.jpg` or `.jpeg` file. Folder names are
   compared as the file system compares them, without case on Windows and macOS.
   Before anything is written, a write is refused with `invalid_argument` when:
   - another folder's `effect.toml` names pack ID `<pack>`, whether or not that pack
     loads (a pack a person installed under another folder name); or
   - `<packs>/<pack>` exists and its `effect.toml` doesn't name pack ID `<pack>`: a
     folder holding a pack of another ID, a manifest that doesn't parse, or no
     `effect.toml` at all. A pack of that ID that fails to load for another reason (a
     shader that doesn't compile) can still be replaced, so an agent can fix it. So a
     write only ever replaces the pack it names, or makes a new folder.
3. **What a write may hold.** At most 64 files, the loader's per-file limits, each
   image at most 2 MiB, and a
   `files` value of at most 3 MiB as sent (text after JSON escaping, images as base64),
   so the whole request, with its envelope, stays under the 4 MiB message limit of
   [ADR-0074](0074-too-large-requests.md). A larger `files` is refused with `too_large`
   before it is decoded.
4. **How a write lands, and how a stopped one is recovered.**
   - Pack writes and deletes run one at a time.
   - A write that finds `.trash/<pack>` with no `<pack>` (an earlier undo failed) first
     renames it back, as the startup recovery does, and answers `internal` if it can't;
     so the only copy of the old version is never removed.
   - A write then removes any leftover `<packs>/.staging/<pack>` and
     `<packs>/.trash/<pack>`; if it can't, it answers `internal` and the old version
     stays. So a write never stages over an earlier write's files, and its first rename
     never meets an existing folder.
   - The files are written into `<packs>/.staging/<pack>`, which the loader never reads
     (it reads only the direct sub-folders that hold an `effect.toml`), and loaded from
     there, checked against the other loaded packs as a poll would check them.
   - Only a pack that loads is swapped in, while the pack watcher is held so no poll
     sees the swap: the old folder is renamed to `<packs>/.trash/<pack>`, the new one
     into its place, then the old one removed. The engine takes the pack loaded from
     staging at once, so the pack never leaves the air between versions.
   - A rename that fails (on Windows, a sharing violation from a virus scanner or an
     editor) is retried a few times for up to a second. If the second rename still
     fails, the first is undone (`.trash/<pack>` back to `<pack>`), the write answers
     `internal`, and the old version stays. If the undo fails too, the write answers
     `internal` saying the pack is off air until the next start, whose recovery puts it
     back. A failure to remove the old folder after a swap is logged and doesn't fail
     the write: the next write removes it first.
   - `pack.delete` removes any leftover `<packs>/.deleted/<pack>`, renames the pack's
     folder there, then removes it, so recovery never mistakes a delete for a stopped
     write.
   - At startup, before the first load, the watcher recovers a swap that stopped
     between the renames: a `.trash/<pack>` with no `<pack>` is renamed back, a
     `.trash/<pack>` beside a `<pack>` is removed, and `.staging` and `.deleted` are
     emptied.
5. **The UI shows who wrote a pack.** `pack.list` reports the client name of the last
   write, and the audit log records every write and delete, so a person can see that an
   agent installed GPU code.
6. **Quarantine follows the shader, not its name** (amends ADR-0087 decisions 2 and 3).
   - A blamed kind's quarantine also records a BLAKE3 digest of each WGSL module it drew,
     as validated: for an effect, its prelude, header and shader text; for a shader
     source, each pass's prelude, header, `common` and pass text.
   - A pack any of whose modules matches a quarantined digest loads with that kind
     quarantined, under whatever pack or kind ID it now has.
   - A quarantine from before this ADR has no digests. It stays keyed by kind ID as
     today, and the first time its pack loads its modules' digests are recorded.
   - Re-enabling stays a person's act (`effect.enable` is still `PEOPLE_ONLY`), and lifts
     the digests with the kind.

<a id="consequences"></a>
## Consequences
- An agent can make, test and fix shader sources and effects in one call each, and see
  compile errors with file and line.
- An MCP agent can now put GPU code on the user's machine without a person installing
  it. What bounds it: naga validation and the shader contract, the size limits above,
  ADR-0093's memory cap, and quarantine after one device reset per distinct shader.
  - A shader that changes a byte each time escapes the digest, so an adversarial agent
    can still cost repeated resets.
  - ADR-0087's third device loss within 60 s fails the outputs, so such an agent can
    stop a stream.
  - No forced GPU reset has yet been run on Windows hardware to check recovery there.
  - The threat model's #349 row stays open for a GPU-time guard.
- An agent can also replace or delete a pack a person installed. That change is on disk
  and undo doesn't reach it; `pack.list`'s last writer and the audit log record it. A
  person who wants a pack kept from agents keeps a copy outside the packs folder.
- The threat model's rows for packs change from "GPU code the user installed" to "GPU
  code a person or a token-holding client installed".
- The protocol gains four commands and a `PROTOCOL_VERSION` bump; the MCP tools follow.
- If the maintainer rejects this, packs stay a person's files (option C), and ADR-0093's
  shader sources still work for packs a person installs.
