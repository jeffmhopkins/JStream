<!-- @doc
id = "SPIKE-S1"
title = "Spike S1: GPU interop"
kind = "guide"
status = "draft"
review_every = "180d"
-->

# Spike S1: GPU interop

Throwaway code for spike S1 in [ROADMAP M0](../../ROADMAP.md#m0), Track B. It checks,
on real hardware, the Windows path that
[ADR-0004](../../docs/adr/0004-wgpu-with-native-interop.md) and
[ADR-0003](../../docs/adr/0003-media-stack-and-codecs.md) fix: shared capture texture,
shared fence, wgpu on D3D12, NV12, Media Foundation H.264, with no CPU copy of frame data
at 1080p60.

It follows [ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline) except for one
deliberate deviation in the convert step. ARCH step 4 writes separate R8 and RG8 targets
in compute shaders and then copies them into an NV12 resource (through hal, or a D3D11
video processor). This spike tries the simpler path first: render passes that write
straight into the NV12 texture's plane views. If that fails, ARCH's step 4 is the
fallback. The results PR must record which convert path was actually proven.

The crate is not a workspace member. It has its own `[workspace]` table and its own
`Cargo.lock`.

Results don't go in this file. They come from the maintainer's run, and a later PR records
them in ARCHITECTURE or an ADR.

<a id="pipeline"></a>
## What it does
<!-- @section
-->

Everything runs on the GPU picked by wgpu (DX12). That is the adapter that drives the
primary display, or the high-performance one if no adapter reports it. Each tick (60 Hz by
default) runs these steps:

1. **Capture:** Windows.Graphics.Capture (free-threaded frame pool, BGRA8) captures the
   primary display. The newest frame is `CopyResource`d into a D3D11 texture that was
   created with a **shared NT handle**. This is the one GPU copy that ADR-0004 expects,
   because WGC pool textures can't be shared.
2. **D3D11 → D3D12:** the shared texture is opened with `ID3D12Device::OpenSharedHandle`
   and wrapped for wgpu with `wgpu::hal::dx12::Device::texture_from_raw` and
   `Device::create_texture_from_hal`. Both APIs use one **shared fence**
   (`ID3D11Device5::CreateFence(SHARED)`, opened in D3D12). D3D11 signals `a`. wgpu waits
   for `a` and signals `b`. The wait and signal are staged with wgpu-hal's
   `Queue::add_wait_fence` and `add_signal_fence`, so wgpu itself issues them around its
   submit.
3. **Convert:** two WGSL render passes write directly into the Y (`R8Unorm`) and CbCr
   (`Rg8Unorm`) plane views of an **NV12** texture (the deviation from ARCH step 4
   described above), using BT.709 limited range and bilinear scaling to the
   encode size. That NV12 texture is also a shared D3D11 texture, one of a ring of 8.
   The command buffer ends by transitioning both shared textures back to `COMMON`
   (wgpu's `TextureUses::PRESENT`) so D3D11 sees them in the state it expects.
4. **Encode:** D3D11 waits for `b`. Then the NV12 texture is wrapped with
   `MFCreateDXGISurfaceBuffer` and sent to a **hardware** H.264 encoder MFT. The encoder
   comes from `MFTEnumEx(MFT_ENUM_FLAG_HARDWARE)`, preferring the MFT on the same adapter
   LUID. It is driven through `IMFDXGIDeviceManager`. Encoding is async, low latency,
   CBR, with no B-frames and a GOP of 2 s.
5. **Output:** the elementary stream (Annex B) goes to `out.h264`. Muxing is out of scope.

If a tick has no new WGC frame, the last frame is converted and encoded again, as the
render loop would do. So the pipeline runs at the target rate even on a still desktop.

<a id="run"></a>
## How to run it (Windows)
<!-- @section
-->

You need Windows 10 1903 or later (Windows 11 recommended), a GPU with a hardware H.264
encoder (NVENC, AMF or Quick Sync, reached through Media Foundation), and the Rust
toolchain from `rust-toolchain.toml`. You don't need admin rights.

```powershell
cd spikes\s1-gpu
cargo run --release -- --seconds 30 --json
```

While it runs, keep something moving at 60 fps on the **primary** display, such as a
60 fps video in fullscreen or a game. WGC only delivers frames when the screen changes,
so a still desktop measures the conversion and encode but not capture. Optionally, run it
again with `--size native` to encode at the display's own size.

Other options are `--fps <N>`, `--bitrate <kbps>`, `--out <path>` and `--size <WxH>`.
Run `cargo run --release -- --help` to list them. To check the output, play `out.h264`
in any player that reads raw H.264, such as VLC.

On stderr the spike prints a `s1 stage` line as it reaches each step: device, capture,
shared textures, encoder setup, first frame. If the process dies of an unhandled
exception (an access violation, for example), it prints an `s1 FAULT` line with the
exception, the faulting module and offset, and a backtrace before Windows ends it. Up to
three `s1 first-chance exception` lines may come first: faults a driver may still handle
itself, so they aren't crashes on their own. Keep stderr when you report a crash.

`spikes\run-spikes.bat` runs all of this unattended. It plays a full-screen animated page
in Edge, runs at 1920x1080 and at native size, and zips the reports, the stderr logs and
the first 6 MB of each H.264 file.

<a id="paste-back"></a>
## What to paste back
<!-- @section
-->

Paste these into the PR or issue:

1. The **JSON line** that `--json` prints. It holds the adapter name, IDs and LUID, the
   driver version, the encoder MFT's friendly name, and the sizes and duration. It also
   has the capture fps, the encoded fps, dropped ticks by cause, the convert and encode
   latency (p50/p95/p99/max), the process CPU and any warnings.
2. What was on screen during the run, such as a 60 fps video, a game, or a still
   desktop.
3. The display's resolution and refresh rate, and whether the PC is a laptop with two
   GPUs.
4. Whether `out.h264` plays correctly. Check that the colours look right, that there is no
   green or pink cast and no tearing, and that motion is smooth.

How to read the numbers:

- **Convert latency** is the time from the CPU submitting a wgpu job to the shared fence
  reaching that job's value. A separate thread measures it with QPC (`Instant`) and
  `SetEventOnCompletion`. It includes queueing, so it is an upper bound on GPU time.
- **Encode latency** is the time from `ProcessInput` until the matching output sample
  arrives.
- **Dropped ticks** have three causes: `late_ticks` (the loop missed its deadline),
  `encoder_busy` (the MFT hadn't asked for more input) and `ring_full` (all 8 NV12 slots
  were still being encoded).
- **Encoded fps** counts only the packets out by the end of the measured window.
  `frames_encoded` also includes the packets from the final drain.
- **CPU** is the process's user and kernel time divided by wall time.

<a id="risks"></a>
## Known risks and uncertainties
<!-- @section
-->

This code was cross-checked from Linux with `cargo check` and `cargo clippy` for
`x86_64-pc-windows-msvc`. Its first run on Windows (Ryzen 9 8945HS, Radeon 780M, 4K
display) crashed with an access violation before printing a report, and stderr was not
kept, so the cause is still open. These are the parts most likely to fail, roughly in
order:

- **NV12 render targets through wgpu.** wgpu only guarantees NV12 for sampling. This code
  wraps a D3D11-created NV12 texture with `RENDER_ATTACHMENT` usage and renders into its
  `Plane0` and `Plane1` views. wgpu-core accepts planar views as render targets, and
  wgpu-hal creates plane-sliced RTVs. If the driver or wgpu rejects this, the fallback is
  ARCH's documented step 4: compute shaders write separate R8 and RG8 targets, which are
  copied into the NV12 resource through hal or the D3D11 video processor.
- **Creating the shared NV12 texture.** This is D3D11 `CreateTexture2D(NV12,
  SHARED | SHARED_NTHANDLE, RENDER_TARGET)` followed by D3D12 `OpenSharedHandle`. Some
  drivers may refuse this combination. The other option is to create the texture in D3D12
  and open it in D3D11.
- **Resource states.** wgpu is told the imported textures start in `COMMON` (`PRESENT`),
  and each frame transitions them back. If the debug layer reports state mismatches, this
  is the place to look.
- **Fence hand-off to the encoder.** D3D11 queues `Wait(b)` on the immediate context before
  `ProcessInput`. This assumes the vendor MFT reads the texture through that device's
  queue after the wait. A vendor MFT that reads it on its own engine without ordering
  against the context could encode stale frames. Look for flicker or old frames in
  `out.h264`.
- **Encoder quirks.** ICodecAPI settings that fail are reported as warnings, not errors.
  `MFT_ENUM_ADAPTER_LUID` is read as a UINT64 or as an 8-byte blob. An output frees its
  NV12 slot and gets an encode latency only when its sample time matches an input's.
  That assumes the MFT keeps input sample times. A warning counts outputs that matched no
  input. If an MFT rewrote every timestamp, no slot would be freed, the ring would fill
  and every later tick would be counted as `ring_full`.
- **Capture.** The pool is not recreated when the display changes resolution. A warning
  is reported and frames keep the original size. `SetIsBorderRequired(false)` is
  best-effort, so the yellow border may show.
- **Hybrid GPUs.** If the display is on another adapter, the `selection` field says so.
  If the encoder MFT is, a warning says so. The numbers then include cross-adapter copies.
- **Not measured:** GPU-only timestamps (the latencies are QPC wall time), power, and
  10-bit/P010.
