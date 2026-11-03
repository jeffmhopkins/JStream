<!-- @doc
id = "ADR-0095"
title = "Agents write effect packs over the protocol and MCP"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0095: Agents write effect packs over the protocol and MCP

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

<a id="decision"></a>
## Decision
1. **Agents may write packs.** The protocol gains `pack.write` and `pack.delete` actions
   and `pack.list` and `pack.read` queries, and `pack` is not a `PEOPLE_ONLY` noun, so
   MCP agents get them. WASM plugins don't: ADR-0069 places them under "never".
   - `pack.write` replaces one pack folder whole with the files it carries (text as
     text, images as base64) and loads it at once. It answers with the pack's kinds, or
     with `invalid_argument` naming each problem's file, line and message, and then the
     old version stays.
   - `pack.delete` removes one pack folder; `pack.list` names the packs and their kinds;
     `pack.read` returns one pack's files.
   - These are actions, not edits: undo doesn't reach the disk.
2. **What a write may hold.** At most 64 files and 3 MiB of file contents, so a pack
   whose images travel as base64 stays under the 4 MiB request limit
   ([ADR-0074](0074-too-large-requests.md)); the per-file limits the loader already has;
   and only `effect.toml`, `.wgsl`, `.png`, `.jpg` and `.jpeg` files, at paths that follow
   the pack's asset-path rules. Nothing outside the packs folder is written.
3. **How a write lands.** The files are written into `.staging/<pack>` in the packs
   folder, which the loader never reads (it reads only the direct sub-folders that hold
   an `effect.toml`), and loaded from there. Only a pack that loads is swapped in: the old folder is renamed
   to `.trash/<pack>`, the new one into its place, and the old one removed. Between the
   two renames the pack is briefly absent, never half written; a poll in that moment
   reports its kinds removed and the next one added again.
4. **Quarantine follows the shader, not its name** (amends ADR-0087 §3 and §4). A blamed
   kind's quarantine also records a digest (BLAKE3) of each WGSL text it drew. A pack
   whose shader matches a quarantined digest loads with that kind quarantined, under
   whatever pack or kind ID it now has. Re-enabling stays a person's act
   (`effect.enable` is still `PEOPLE_ONLY`), and lifts the digests with the kind.
5. **The UI shows who wrote a pack.** `pack.list` reports the client name of the last
   write, and the audit log records every write and delete, so a person can see that an
   agent installed GPU code.

<a id="consequences"></a>
## Consequences
- An agent can make, test and fix shader sources and effects in one call each, and see
  compile errors with file and line.
- An MCP agent can now put GPU code on the user's machine without a person installing
  it. What bounds it: naga validation and the shader contract, the size limits above,
  ADR-0093's memory cap, and quarantine after one device reset per distinct shader. A
  shader that changes a byte each time escapes the digest, so an adversarial agent can
  still cost repeated resets; the threat model's #349 row stays open for a GPU-time
  guard.
- The threat model's rows for packs change from "GPU code the user installed" to "GPU
  code a person or a token-holding client installed".
- The protocol gains four commands and a `PROTOCOL_VERSION` bump; the MCP tools follow.
- If the maintainer rejects this, packs stay a person's files, and ADR-0093's shader
  sources still work for packs a person installs.
