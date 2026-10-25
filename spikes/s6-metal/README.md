<!-- @doc
id = "SPIKE-S6"
title = "Spike S6: IOSurface and Metal interop"
kind = "guide"
status = "draft"
review_every = "180d"
-->

# Spike S6: IOSurface and Metal interop

Throwaway code for the spike that
[ADR-0063](../../docs/adr/0063-macos-platform-support.md) decision 8 asks for before the
macOS crate gets GPU frames. It checks, on a real Mac, the path that decision and
[ADR-0004](../../docs/adr/0004-wgpu-with-native-interop.md) lay down:
- IOSurface-backed pixel buffers wrapped as Metal textures, with no copy;
- those textures adopted into the engine's wgpu device through wgpu-hal;
- the engine rendering NV12 into IOSurface encode targets that VideoToolbox reads.

Above all, it decides how that hand-off is synchronised. It measures two choices, plus a
variant of one, against a control that has no synchronisation.

It is modelled on [spike S1](../s1-gpu/README.md#pipeline), which proved the Windows path.

The crate is not a workspace member. It has its own `[workspace]` table and its own
`Cargo.lock`.

Results don't go in this file. They come from the maintainer's run, and a later PR records
them in [ARCH › GPU pipeline](../../docs/ARCHITECTURE.md#gpu-pipeline) and ADR-0063.

<a id="pipeline"></a>
## What it does
<!-- @section
-->

Everything runs on the Mac's Metal GPU through wgpu. There are two wgpu devices on the
same adapter:
- **The producer** stands in for ScreenCaptureKit or a video decoder: something else on
  the GPU that writes a frame into an IOSurface.
- **The engine** is JStream's device.

Each device has its own `MTLCommandQueue` and its own `MTLTexture` over each IOSurface.
So, as with a real capture or decoder frame, Metal tracks no hazards between them: only
the synchronisation under test orders the engine's read after the producer's write.

Each tick (60 Hz by default) runs frame `n` through one slot of a ring of 8:

1. **Produce:** the producer renders frame `n` into the slot's BGRA IOSurface (a
   `CVPixelBuffer` made with `kCVPixelBufferIOSurfacePropertiesKey` and Metal
   compatibility). Every pixel first runs `--load` iterations of busy work, so the frame
   takes long enough on the GPU for an early read to show. The frame's code (3 to 252,
   from `n`) goes in a 64×64 **import probe** block at the top left.
2. **Import:** the engine wraps the same IOSurface:
   - `newTextureWithDescriptor:iosurface:plane:`, then `wgpu::hal::metal::Device::texture_from_raw` and `create_texture_from_hal`;
   - it converts the frame to NV12 (BT.709 limited range) in the slot's NV12 IOSurface, whose Y and CbCr planes are wrapped the same way as `R8Unorm` and `Rg8Unorm` render targets;
   - the import probe is copied exactly from the source;
   - a second 64×64 **export probe** block gets the engine's own code for frame `n`.
3. **Hand over:** a consumer thread waits for the engine's frame, then:
   - reads both probes on the CPU, with the IOSurface locked read-only;
   - gives the NV12 pixel buffer itself to a VideoToolbox H.264 session, as the engine's encoder will once encode targets are IOSurfaces.

   The slot is free again once VideoToolbox's output callback has run for that frame.

The probes tell the consumer what went wrong:
- **Export probe wrong:** the hand-over to the consumer happened too early (`stale_export`).
- **Export probe right, import probe wrong:** the engine read the producer's IOSurface too early (`stale_import`).
- **A value that is no frame's code:** an unwritten or torn frame (`garbled`).

The `--sync` modes:

| Mode | Producer → engine | Engine → consumer |
|---|---|---|
| `none` | nothing (the control) | nothing |
| `completion` | the CPU waits for the producer's submission (`Device::poll` with its index), then submits the engine's work | the CPU waits for the engine's submission the same way |
| `event` | the producer's submit signals `MTLSharedEvent` `in` to `n + 1`; the engine's submit waits for it on the GPU (wgpu-hal `add_signal_event` and `add_wait_event`) | the engine's submit signals `out` to `n + 1`; the consumer waits with `waitUntilSignaledValue:timeoutMS:` |
| `event-strict` | as `event`, with wgpu-hal's `enable_strict_event_sync` on the engine's queue | as `event` |

`event-strict` is in the list because of how wgpu-hal stages a wait. By default it encodes
the wait on its own command buffer, committed just before the submit's own. Its
documentation calls that "best-effort under cross-CB GPU concurrency", because Metal may
overlap command buffers that share no tracked resource. Strict mode makes every command
buffer of the submit wait on the event: one more `encodeWaitForEvent` at the start of each
command buffer (wgpu-core's pending-writes and transit buffers included, so several per
submit here), plus a relay signal on the internal wait buffer, which `event` already
commits every frame. It adds no command buffer over `event`.

<a id="run"></a>
## How to run it (macOS)
<!-- @section
-->

You need:
- macOS 14 or later, on Apple Silicon. On an Intel Mac, note it in the paste-back (see Known risks).
- The Rust toolchain from `rust-toolchain.toml`.
- Xcode's command-line tools, for the linker (`xcode-select --install`).

It asks for no permissions: there is no screen capture, camera or microphone.

```sh
cd spikes/s6-metal
cargo run --release -- --json
```

That runs the four modes in turn, 60 s each at 1920×1080 and 60 fps, and prints one JSON
line per mode. It writes `out-<mode>.h264` for each. Keep the Mac on mains power with
nothing else heavy running. Then:

1. **If `none` shows `stale_import` and `garbled` both 0,** the control hasn't caught a
   race, so the check can't tell the modes apart. Raise the load and run all four again,
   for example `--load 256`, then `--load 1024`. Use the first load at which `none` shows
   early reads, as long as it still keeps 60 fps. If `none` shows none even at 1024, say
   so: that is a finding in itself.
2. **Run the whole set a second time** at that load, to see whether any non-zero count
   repeats.

Other options are `--sync <mode>` (one mode only), `--seconds <N>`, `--size <WxH>`,
`--fps <N>`, `--bitrate <kbps>` and `--out <prefix>`. Run `cargo run --release -- --help`
to list them.

On stderr the spike prints an `s6 stage` line as it reaches each step: devices, surfaces,
encoder, running, draining. Keep stderr if it fails.

<a id="paste-back"></a>
## What to paste back
<!-- @section
-->

Paste these into the issue:

1. Every JSON line from both runs, and the `--load` each used.
2. The Mac's model and chip, and the macOS version. The JSON has them too, from `sysctl`
   and `sw_vers`.
3. For `out-event.h264` and `out-completion.h264`: whether each plays. Check that the
   picture is a moving pattern with no tearing, no flashes of an older frame and no
   colour cast. To play one, use `ffplay out-event.h264`, or VLC, which reads raw H.264.

How to read the numbers:

- **`stale_import`, `stale_export`, `garbled`:** frames the consumer caught being read or
  handed over before they were ready. A correct mode has all three at 0.
- **`sync_timeouts`:** waits that took longer than 2 s. A correct mode has 0.
- **`latency_ms`:** from the tick thread starting frame `n` to the consumer holding it,
  done. It includes queueing.
- **`tick_work_ms`:** the tick thread's time per frame. In `completion` it includes the
  CPU wait for the producer, which the event modes don't have.
- **`dropped`:** `late_ticks` means the loop missed a deadline by more than a frame.
  `ring_full` means all 8 slots were still in use, by the GPU or by VideoToolbox.
- **`encoder_hardware`:** whether VideoToolbox gave a hardware session.
- **`same_mtl_device`:** whether both wgpu devices are on one `MTLDevice`, as expected on
  a single-GPU Mac.

<a id="decision"></a>
## What decides
<!-- @section
-->

A mode **passes** when, in both runs at the load where `none` shows early reads:
- `stale_import`, `stale_export`, `garbled` and `sync_timeouts` are all 0;
- `out-<mode>.h264` plays cleanly;
- `dropped.late_ticks` is under 1% of `ticks`.

Then, in this order:

1. **`event` passes,** and either `completion` fails or `event`'s `latency_ms.p95` is at
   most 2 ms worse than `completion`'s: the macOS backend uses shared events, staged with
   `add_wait_event` and `add_signal_event`, the counterpart of S1's shared fence.
   ARCH › GPU pipeline records it, with `event`'s `latency_ms.p95`. If `event` passes but
   is more than 2 ms worse than a passing `completion`, the backend uses command-buffer
   completion instead, with CPU waits, at the latency and `tick_work_ms` measured.
2. **`event` fails but `event-strict` passes:** shared events, with
   `enable_strict_event_sync` on the engine's queue.
3. **Neither event mode passes but `completion` does:** command-buffer completion, with
   CPU waits, at the latency and `tick_work_ms` measured.
4. **No mode passes:** none of these is safe as written. The failing counts say where:
   import, hand-over, or torn frames. The backend stays on CPU frames until a fix is
   found.

If both event modes pass, `event` wins: it costs less.

<a id="risks"></a>
## Known risks and uncertainties
<!-- @section
-->

This code was only type-checked and linted from Linux (`cargo clippy --target
aarch64-apple-darwin`). It has never been linked or run, so expect a first-run fix or two.
The parts most likely to fail, roughly in order:

- **Wrapping IOSurface planes.** The textures use the descriptor's default storage mode
  and only the usage they need: shader read for the source, render target for the
  others. If Metal refuses a plane, the error names it. Try `setStorageMode(Shared)` in
  `Gpu::import`.
- **wgpu accepting the textures.** `create_texture_from_hal` is given
  `TextureUses::UNINITIALIZED`, since every frame overwrites the whole texture. wgpu
  validation errors would name the texture's label.
- **The CPU probe read.** It locks the IOSurface read-only and reads one byte of each
  probe. If `IOSurfaceLock` itself waited for pending GPU writes, `none` would never show
  `stale_export`. The import check doesn't depend on that, because the engine reads
  through the GPU.
- **Intel Macs with a discrete GPU.** Writes to managed textures may need a blit to reach
  the IOSurface's system memory. Results from such a Mac are worth having, but they
  don't decide the Apple Silicon path.
- **VideoToolbox and the encode targets.** The pixel buffers are made by the spike, not
  by the session's pool. If VideoToolbox refuses them, or copies them, the warnings and
  `frames_encoded` show it.
- **Not covered:**
  - real ScreenCaptureKit or decoder frames, which are complete when delivered, so the
    producer here is the harder case;
  - GPU-only timestamps, since the latencies are wall time;
  - P010 and 10-bit;
  - power.
