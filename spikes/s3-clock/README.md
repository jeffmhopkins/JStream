<!-- @doc
id = "SPIKE-S3"
title = "Spike S3: media clock and audio drift"
kind = "guide"
status = "draft"
review_every = "180d"
-->

# Spike S3: media clock and audio drift

Throwaway code for spike S3 in [ROADMAP M0](../../ROADMAP.md#m0), Track B: "WASAPI mic
+ loopback timestamps mapped to QPC, drift measured over an hour, adaptive resampler
validated". It checks, on real devices, what
[ADR-0014](../../docs/adr/0014-media-clock.md) and
[ARCH › Threads, clock and sync](../../docs/ARCHITECTURE.md#threading) assume: that
WASAPI's packet timestamps can be mapped onto one QPC media clock, how far real devices
drift against it, and that a PI-steered adaptive resampler keeps each device on the
engine's time line. Audio uses WASAPI, including process loopback, as
[ADR-0008](../../docs/adr/0008-windows-support-policy.md) says.

The crate is not a workspace member. It has its own `[workspace]` table and its own
`Cargo.lock`.

Results don't go in this file. They come from the maintainer's run, and a later PR records
them ([R-DOC-004](../../docs/RULES.md#R-DOC-004)): in
[ARCH › Threads, clock and sync](../../docs/ARCHITECTURE.md#threading), and in a new ADR
if they change ADR-0014's decision. Accepted ADRs are never edited
([R-DOC-009](../../docs/RULES.md#R-DOC-009)).

<a id="what"></a>
## What it does
<!-- @section
-->

It opens two WASAPI capture streams in shared, event-driven mode, each on its own thread
registered with MMCSS "Pro Audio":

- **mic:** the default capture endpoint (console role), in its mix format;
- **loopback:** loopback of the default render endpoint, which is everything the desktop
  plays. With `--process <pid>`, it captures that process tree's audio through process
  loopback (`AUDIOCLIENT_ACTIVATION_PARAMS`) instead, as 48 kHz stereo float.

A third thread plays **silence** on the default render endpoint (the keep-alive). Without
it, loopback delivers no packets while nothing else plays. `--no-keepalive` turns it off.

For every packet, `IAudioCaptureClient::GetBuffer` returns the device position of its
first frame and that frame's QPC time in 100 ns units (`pu64QPCPosition`). The spike uses
that unit as its media clock. Then, per stream:

1. **Continuity.** A packet that starts where the last one ended is continuous. A forward
   jump means frames were lost. If the QPC time moved by the same amount (within 5 ms),
   the gap is filled with silence and the mapping continues. Otherwise, and when the
   position goes backwards or jumps more than 2 s, the mapping restarts with a new
   segment. `DATA_DISCONTINUITY`, `SILENT` and `TIMESTAMP_ERROR` flags are counted. A
   packet flagged `TIMESTAMP_ERROR`, or with a zero QPC time, is not used for timing.
2. **Drift.** A running least-squares fit of device time (position divided by the
   nominal rate) against QPC time gives the device's real rate. Its drift is reported in
   ppm, with a standard error and the timestamp jitter around the line. Each segment gets
   its own intercept, so a glitch never biases the slope. A point more than 20 ms off the
   line is an outlier and isn't fitted. A run of 50 outliers restarts the segment, with
   the 50th as its first point. Only stamps the fit accepted are used for timing below:
   an outlier, like a flagged stamp, still has its samples resampled, but the resampler
   extrapolates its time from the last good stamp.
3. **Mapping.** The fitted line maps any sample position onto the QPC media clock. The
   report shows how far that smoothed mapping sits from the raw packet stamps that the
   fit accepted.
4. **Adaptive resampler.** The stream's first one or two channels go through rubato's
   asynchronous sinc resampler (128 taps, cubic) to the engine rate (`--rate`, default
   48 kHz) on the QPC clock. After each 10 ms chunk, the timing error is
   `e = output frames / engine rate − QPC time of the input consumed`, relative to the
   first measurement. It is smoothed (0.5 s) and fed to a PI controller (0.02 Hz,
   damping 0.707, correction clamped to ±5000 ppm) that sets the resampler's ratio. A
   consumer that pulls the engine rate per QPC second would see its buffer fill drift by
   exactly `e`. The **residual** is that error once the loop has settled. The question is
   whether it stays within ±1 ms over the run.

The mic-vs-loopback **relative drift** comes from the two fitted drifts. The relative
residual after resampling is the difference of the two streams' residuals, since both
are steered to QPC.

Once a second the main thread writes one row of `drift.csv`. Every `--progress` seconds
it prints a line to stderr. At the end it prints the report to stdout (`--json` for one
JSON object). The residual statistics, per stream and relative, skip the first
`--settle` seconds, which defaults to 60, or a quarter of a shorter run. Both count
from one QPC origin, taken just before the streams open.

<a id="run"></a>
## How to run it (Windows)
<!-- @section
-->

You need Windows 11 with a microphone and an output device, and the Rust toolchain from
`rust-toolchain.toml`. You don't need admin rights. First run a two-minute smoke test:

```powershell
cd spikes\s3-clock
cargo run --release -- --minutes 2 --json
```

If it ends with a JSON line and no `"error"` strings, run the hour and save the JSON:

```powershell
cargo run --release -- --json > s3.json
```

While it runs:

- **Play audio** for the whole run, such as a long music playlist or video, at any volume.
  The keep-alive keeps loopback going without it, but real playback is the realistic case.
- **Leave the devices alone.** Don't unplug them, change the default devices or change
  their formats in Sound settings during the run.
- **Keep the PC awake.** Sleep or hibernation ends the measurement.
- Progress lines appear in the console every 10 s. **Ctrl+C** stops early and still
  writes the report and the CSV.

The most informative setup has the mic and the speakers on **different clocks**, for
example a USB mic or headset with the speakers on the motherboard audio. If both run on
one audio interface, the relative drift will be near zero, which is also worth knowing.
To try process loopback, find a player's PID in Task Manager (Details tab) and add
`--process <pid>`. Other options are `--minutes`, `--progress`, `--csv`, `--settle`,
`--rate` and `--no-keepalive`; run `cargo run --release -- --help` to list them.

<a id="paste-back"></a>
## What to paste back
<!-- @section
-->

Paste these into the PR or issue:

1. The **JSON line** (`s3.json`). It holds the Windows build, each stream's device name
   and format, packet and glitch counts, drift (ppm, stderr, jitter, accumulated offset),
   the mapping residual, the resampler's correction and residual (max and RMS), the
   relative drift and residual, `resampler_within_1ms`, CPU use and any warnings.
2. **`drift.csv`**: attach the file. It has one row per second, about 3600 rows.
3. What the devices are: the mic (USB, built-in, headset) and the output (motherboard,
   USB DAC, HDMI), and whether they share one interface.
4. What played during the run, and anything you heard: clicks, dropouts, the mic or
   desktop audio stopping.

How to read the numbers:

- **`drift.ppm`** is positive when the device delivers more samples per real second than
  its nominal rate (its clock is fast against QPC). Crystal oscillators are typically
  within ±100 ppm. **`offset_ms`** is that drift accumulated over the run: how far the
  device would have drifted from the media clock without resampling.
- **`jitter_ms`** is the standard deviation of the packet stamps around the fitted line.
  **`mapping_residual_ms`** is the same thing seen from the mapping. Both bound how
  precisely one packet can be placed on the media clock.
- **`resampler.correction_ppm`** should settle near minus `drift.ppm`.
- **`residual_ms`** is the smoothed timing error that ADR-0014's resampler leaves.
  `residual_raw_ms` adds the unsmoothed packet jitter on top.
- **Glitches** are discontinuity flags (not counting one on the first packet, which
  WASAPI sets routinely), gaps filled with silence, segment resets and timestamp errors.
  `wait_timeouts` counts 200 ms waits with no event.
- **CPU** is the process's user and kernel time divided by wall time. It includes two
  sinc resamplers, the keep-alive and the fits.

<a id="risks"></a>
## Known risks and uncertainties
<!-- @section
-->

The Windows code was cross-checked from Linux with `cargo check` and `cargo clippy` for
`x86_64-pc-windows-msvc`. **It has never been run.** The platform-independent logic
(QPC conversion, continuity, the drift fit, the PI loop, the resampler on simulated
drifting devices, and per-packet processing with glitches) is unit-tested with
`cargo test` on any OS. These are the parts most likely to surprise at the first run:

- **Loopback timestamps.** Loopback packets are produced by the render engine. Their
  device position and QPC time may track the render endpoint's clock in a way that
  differs from a capture endpoint's. They may also be coarse, or flagged
  `TIMESTAMP_ERROR`. The jitter and outlier counts will show it.
- **Event-driven loopback.** Loopback with `AUDCLNT_STREAMFLAGS_EVENTCALLBACK` relies on
  Windows signalling the event for loopback, which older Windows versions did not do.
  Many `wait_timeouts` with few packets would point here.
- **Process loopback.** The activation runs from an MTA thread and waits up to 5 s for
  the completion handler. Its format is fixed (48 kHz stereo float, with the engine's
  converter), and its device position and QPC stamps are virtual. They may not reflect
  any hardware clock, so its drift may read as exactly zero. It is optional for S3.
- **The keep-alive's effect.** Rendering silence keeps the render endpoint active. This
  is what OBS does, but it means loopback never sees an idle endpoint.
- **Mix formats.** Only 32-bit float and 16, 24 or 32-bit integer PCM are converted. Any
  other format fails at start-up with a message.
- **Controller tuning.** The 0.02 Hz loop settles in about a minute. A device beyond
  roughly 200 ppm overshoots 1 ms during settling, but the residual statistics skip the
  settling time. A device whose drift changes faster than the loop can follow would show
  as a growing residual in the CSV.
- **Not measured:** device switching, sleep and resume, exclusive mode, and the
  engine's own output devices (render-side resampling).
