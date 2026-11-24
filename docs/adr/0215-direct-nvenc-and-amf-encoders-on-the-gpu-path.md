<!-- @doc
id = "ADR-0215"
title = "Direct NVENC and AMF encoders on the Windows GPU path, before the Media Foundation MFTs"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0215: Direct NVENC and AMF encoders on the Windows GPU path, before the Media Foundation MFTs

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0140](0140-source-types-and-decoding-scope-v3.md) has Windows encode video through
Media Foundation's hardware MFTs, which reach NVIDIA, AMD and Intel through one D3D11
path, and calls direct NVENC and AMF "later optimizations, each needing measurements".
The roadmap's F7 item carried that condition.

The MFT is a wrapper over the same engines. NVIDIA's MFT drives NVENC and AMD's drives
AMF, but through Media Foundation's sample, event and attribute model, which costs an
extra layer on every frame and hides most of the vendor's settings: the MFT exposes
`ICodecAPI`'s generic quality-versus-speed knob where NVENC has its P1 to P7 presets
with a tuning, and AMF its usages and quality presets. What OBS ships for streaming on
those GPUs is the direct path.

The maintainer chose to land the direct encoders in alpha.13 rather than wait for a
measurement to justify them first. So the measurement comes with them instead: both
paths stay in the build, one variable switches between them, and the platform tests run
the same live 1080p30 feed through each, so a run on the hardware compares them.

Neither vendor offers a maintained Rust binding that fits
([R-UNSAFE-004](../RULES.md#R-UNSAFE-004)): the NVENC crates generate their bindings
with `bindgen` against the CUDA toolkit at build time, and AMF has none. Both APIs are
small C interfaces (NVENC a table of functions, AMF COM-like vtables) published as
MIT-licensed headers, and the engine calls only a few dozen of their entries. VA-API on
Linux is reached the same way ([ADR-0060](0060-linux-platform-support.md)).

<a id="decision"></a>
## Decision
1. **Where.** On the Windows GPU path only (`Codecs::on_context`), whose encoders read
   the context's encode targets. The GPU's vendor picks the encoder: NVENC on an
   NVIDIA adapter, AMF on an AMD one. Intel stays on Media Foundation, as ADR-0140 has
   it (QSV's dispatcher is C++). The CPU path is unchanged.
2. **Order.** The direct encoder is tried first. If it can't open (no runtime, a driver
   too old, a codec or size the hardware doesn't encode), the Media Foundation MFT for
   the same GPU opens as before, and the engine logs why the direct one didn't. Both are
   hardware encoders, so `HardwarePreference` treats them alike, and a configuration
   that avoids hardware opens neither.
3. **Runtime, not a bundled library.** Each is the vendor's runtime DLL that its display
   driver installs (`nvEncodeAPI64.dll`, `amfrt64.dll`), loaded from System32 when an
   encoder opens. Nothing is shipped with JStream, so ADR-0140's rule against bundled
   codec libraries holds: this is the "OS or GPU-vendor path" it allows.
4. **Bindings.** Hand-written declarations of only the entries the engine calls, in
   `jstream-platform-windows`, from the vendors' MIT-licensed headers (NVENC API 12.0,
   which drivers from 522 on offer; AMF's C interface), with their notices kept beside
   them. Every struct's size and the offsets the engine uses are asserted at compile
   time against the layouts the headers give under the MSVC ABI, on every platform.
5. **Same contract as the MFT.** Same configuration (CBR or peak-constrained VBR, the
   keyframe interval as the GOP, no B-frames so `dts == pts`, BT.709 limited range),
   same framing (Annex B with parameter sets on every IDR frame, AV1 as low-overhead
   OBUs), same input path (each encode target copied on the GPU into the encoder's own
   texture, so the target goes back to its producer at once), and each packet gets its
   frame's exact `pts` back. The quality settings are the vendors' streaming defaults
   as OBS uses them (NVENC P5 with the high-quality tuning; AMF's transcoding usage).
6. **A switch, not a setting.** `JSTREAM_DIRECT_ENCODERS=off` keeps every GPU encoder on
   Media Foundation. Like `JSTREAM_GPU_PRIORITY` ([ADR-0195](0195-the-engine-runs-at-top-priority.md))
   it describes the machine, not a show, so it is an environment variable and not a
   protocol setting. `CodecInfo::name` says which encoder opened ("… (direct)"), which
   `output.status` and `profile.get` already report.

<a id="consequences"></a>
## Consequences
- NVIDIA and AMD machines encode through the vendor's own API by default, with the
  MFT as the fallback, and a run can compare the two with one variable.
- The crate carries two more hand-written FFI layers to keep in step with the vendors'
  headers. The layout asserts catch a wrong size or offset at build time, but not a
  wrong meaning, so each needs a `hw-tests` run on its vendor's GPU before it is trusted:
  AMF on the streaming PC's Radeon 780M, NVENC on an NVIDIA GPU when one is at hand.
- A driver older than NVENC API 12.0 or an AMF runtime too old for the codec fails the
  direct open and falls back to the MFT, with the reason in the log.
- ADR-0140's "each needing measurements" is met by the comparison tests rather than
  before the build. If a run shows the MFT ahead on a vendor, the default can flip for
  that vendor without removing the code.
