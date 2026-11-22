<!-- @doc
id = "ADR-0175"
title = "Ship Microsoft's DirectX Shader Compiler in the Windows zip"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0175: Ship Microsoft's DirectX Shader Compiler in the Windows zip

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Each `output.start` creates a new GPU device, and on Direct3D 12 wgpu compiles every shader
again for it. With FXC, the HLSL compiler built into Windows (`d3dcompiler_47.dll`), that is
about 4.4 s on the render thread on a mid-range laptop GPU. The stream starts with a stall
of 2 to 4 s.

[wgpu](https://github.com/gfx-rs/wgpu)'s DX12 backend can compile with Microsoft's
[DirectX Shader Compiler](https://github.com/microsoft/DirectXShaderCompiler) (DXC)
instead. Its default compiler setting, `Dx12Compiler::Auto`, loads `dxcompiler.dll` through
the normal Windows DLL search when the DLL is there, and uses FXC when it isn't. The engine
builds its wgpu instance without choosing a compiler, so Auto applies and no engine code
changes. A DLL that loads but is too old (wgpu needs v1.8.2502 or later) is an instance
error, not a fallback, so the release must be pinned. With DXC v1.8.2502 beside the daemon,
the same machine's first-frame stall fell from 4.0 s to 1.87 s, with the same picture and the
same steady GPU time per frame.

DXC is not part of Windows. Using it means shipping two Microsoft binaries in the portable
zip ([ADR-0066](0066-portable-windows-distribution-v2.md)): `dxcompiler.dll` and `dxil.dll`.
The release zip
([v1.8.2502](https://github.com/microsoft/DirectXShaderCompiler/releases/tag/v1.8.2502))
holds three licence files. Its notes apply `LICENSE-MIT.txt` to one header and
`LICENSE-LLVM.txt` (University of Illinois/NCSA, permissive) to every other file. It also
holds `LICENSE-MS.txt`, the Microsoft Software License Terms for the compiler. Those terms
let an application redistribute the compiler's object code, on conditions:

- the application adds significant primary functionality to it;
- distributors and end users agree to terms that protect it and Microsoft at least as much
  as Microsoft's own terms;
- the distributor indemnifies Microsoft against claims about the application, except claims
  based solely on the unmodified compiler;
- Microsoft's trademarks are not used to suggest endorsement, and the compiler is not made
  subject to a licence that would require its source to be disclosed.

The release's notes and its licence files do not say plainly which of the two licences
governs which DLL, so this ADR takes the stricter reading: the Microsoft terms apply to both.

Redistributing a third-party binary under non-permissive terms is a licensing decision, so
this ADR is maintainer tier ([R-GOV-005](../RULES.md#R-GOV-005)). The maintainer approved
shipping the compiler DLL with the Windows build in the project chat.

<a id="decision"></a>
## Decision
1. **The Windows zip ships DXC.** `dxcompiler.dll` and `dxil.dll` from Microsoft's official
   DXC release v1.8.2502, for the zip's architecture (x64, or arm64 when built on arm64),
   sit at the top of the zip beside `jstream.exe` and `jstream-daemon.exe`, where Windows'
   DLL search finds them for every host that opens a compositor.
2. **FXC stays the fallback.** No engine code chooses a compiler. Without the two DLLs (a
   zip built with `--no-dxc`, or a user who deletes them), wgpu uses FXC as before, and
   JStream runs unchanged apart from the slower start.
3. **Pinned and checked twice.** CI downloads the release zip from Microsoft's GitHub
   release and checks its SHA-256. `cargo xtask dist` checks each DLL and licence file it
   copies against a pinned BLAKE3 hash, and refuses a DLL that imports the Visual C++
   runtime. `dist` needs `--dxc-dir <unzipped release>` or an explicit `--no-dxc`, so a
   build never drops DXC silently. Updating DXC is a PR that changes the URL and the hashes.
4. **Notices.** The zip's `dxc` folder holds the release's three licence files as they are
   and a `DXC-VERSION.txt` naming the release, where it came from, its sources and which
   licence applies. `README.txt` says what the DLLs are and that FXC is used without them.
   Each release's notes name the DXC release, as they name the CEF build
   ([ADR-0021](0021-browser-source.md)).
5. **The licence policy names the exception.** [R-DEP-001](../RULES.md#R-DEP-001) names DXC
   beside the CEF runtime as a shipped binary outside the Cargo licence allow list.

<a id="consequences"></a>
## Consequences
- **Faster output start on Windows**, about 2 s sooner on the measured machine, with no
  engine change and no change on other platforms.
- **The zip grows** by about 19 MB.
- **JStream's Windows zip is no longer covered by permissive licences alone.** Whoever
  redistributes the zip passes on Microsoft's terms for the DXC files, and the indemnity
  condition in those terms falls on the distributor. Every other part of the zip keeps its
  licences.
- **Upkeep:** a newer wgpu may need a newer DXC. A wgpu update that raises its minimum
  shows up as an instance error on Windows CI, and the fix is a PR that bumps the pinned
  release and its hashes.
- **Not checked yet:** whether DXC changes any golden image on Windows CI (WARP). The test
  suites don't load the zip's DLLs, so they still compile with FXC.
