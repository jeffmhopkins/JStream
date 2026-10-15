<!-- @doc
id = "CRATE-jstream-audio"
title = "jstream-audio"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-audio

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-audio/src/**", "crates/jstream-audio/tests/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-audio", "docs/ARCHITECTURE.md#threading"]
verified = { hash = "b3:d897f78972f32e13", by = "agent:claude@session_01SZXaqcgUA1wWCDzxs3hWJ6", note = "Dependency only (E1 changed ARCH threading): the render snapshot and collector are the render thread's; the audio crate's mixer, rings and no-arc-swap statements still hold" }
-->

The engine's real-time audio mixer. Layer 2: no I/O and no platform code. It depends
on [`jstream-media`](../jstream-media/README.md) for formats and timestamps, and on
[`jstream-platform`](../jstream-platform/README.md) only for the audio input handler
it implements. This first cut covers what [ROADMAP M1a](../../ROADMAP.md#m1) needs
(a WASAPI microphone and desktop loopback mixed for the AAC encoder): adaptive
resampling of each input onto the engine time line, one stereo bus with a gain per input,
and metering. In [ADR-0028](../../docs/adr/0028-audio-model-v2.md)'s model each input
is a source's audio stream with its fader and mute, and the stereo bus is the master bus,
which every input implicitly sends to at 0 dB. The send matrix, further buses, activity
gains (audio that follows the program), filter chains and latency compensation,
and monitor taps come later. The true-peak and loudness meters
([Meters](#meters)) and the filter kernels ([Filters](#filters)) exist as DSP but
don't run in the mixer yet.

**Time line.** Engine frame `n` is media time `n / rate` on the media clock
([ADR-0014](../../docs/adr/0014-media-clock.md)), with `rate` the engine rate (48 kHz by
default). Each input is resampled so that the audio it captured at media time `t` lands on
engine frame `t · rate`. Inputs therefore line up with each other and with anything else
stamped on the clock, and no device is the master.

**API.**
- `Mixer::new(MixerConfig)`: engine rate, block size (default 1024 frames, one AAC
  access unit), latency (default 1440 frames, 30 ms) and ring size (default 16384
  frames) per input. `MixerConfig::validate` checks the ranges.
- `Mixer::add_input(format)` takes the stream's format (device rate, mono, stereo or 5.1,
  interleaved `f32`; 8 to 192 kHz at a 48 kHz engine) and returns an `InputProducer`, which
  implements `jstream_platform::AudioInputHandler` and goes to `AudioIo::open_input` as
  the stream's handler, and an `InputControl` for the control plane. It allocates, so it
  is called while setting up. The control plane keeps an `InputControl` until the
  stream is closed and the mixer is dropped, so the input's shared state is freed there
  (see [Real-time design](#realtime)).
- `Mixer::poll(now, out)` mixes the next block once the media clock `now` has passed its
  end by the latency, into `out` (interleaved stereo), and returns its `Block` (first
  frame, length, and `pts` in ticks of `1 / rate`). The first call starts the time line
  at `now` minus the latency. `Mixer::mix(frame, out)` mixes a given block regardless of
  the clock. A mixer that falls more than half a ring behind the clock jumps to the
  newest due block and counts the skipped frames.
- `InputControl` and `BusControl` set gain (linear or dB, 0 to +24 dB) and mute, ramped
  over one block. They read meters (per channel, post-gain: the peak held until read, and
  the last block's RMS) and counters (`InputStats`, `BusStats`): packets, discontinuities,
  gap, late, overrun and underrun frames, resets, re-anchors, timestamp outliers, the
  measured drift and stamp jitter, the controller's correction and timing error, and
  whether the stream failed.
- `meter::TruePeakMeter` and `meter::LoudnessMeter`: the true-peak and BS.1770 loudness
  meters, not yet run by the mixer ([Meters](#meters)).
- `filter::AudioFilter` and its kernels `Gain`, `NoiseGate`, `Compressor` (with a
  sidechain key), `Limiter` and `Delay`, not yet run by the mixer ([Filters](#filters)).

**Errors** (`AudioError`): an invalid configuration (including a loudness meter's
rate, or a filter's rate or settings), an unsupported input rate or sample layout, or a wrong output length. No variant allocates.

**Tests.** The unit tests in each `src/rt/` module cover:
- ring: underrun, drop-oldest overrun, late frames, forward jumps, and a two-thread test
  that no torn frame is ever accepted;
- resampler: tone level and purity at 1 and 15 kHz, anti-aliasing when downsampling,
  zero delay, output rate under a correction, long silences;
- the controller, drift fit, continuity and downmix coefficients, ported from S3 with
  S3's tests;
- the input's anchoring, outlier runs, discontinuities and silent packets;
- the mixer's gains, ramps, meters, under- and overrun counters, and clocked polling.

`tests/sim.rs` runs simulated devices through the public API on a simulated 100 ns
clock, with no real devices or threads ([R-TEST-004](../../docs/RULES.md#R-TEST-004)).
The time-line error is measured from the audio itself: each device plays a tone defined
in real time, and its phase in every second of the mix is compared with where it should
be.
- **Drift case** (four minutes): a 44.1 kHz mono mic at +80 ppm with ±0.3 ms stamp
  jitter, and a 48 kHz 5.1 loopback at −150 ppm with ±0.06 ms. After two minutes both
  are within 0.05 ms of the time line (simulated: 0.014 and 0.004 ms at worst) and
  within 1 ms during settling, at the right gain. The fit finds each drift within 1 ppm,
  and nothing under- or overruns after the start.
- **Glitch case:** a lost packet, a 150 ms stall, a single bad timestamp and a stream
  restart. Each is counted as it should be, and the audio is back on the time line
  afterwards.

The crate is built with `opt-level = 3` in the dev profile (root `Cargo.toml`). Debug
builds of the app need a resampler fast enough for a real-time thread, and the
simulations need it to finish in seconds.

<a id="realtime"></a>
## Real-time design
<!-- @section
covers = ["crates/jstream-audio/src/rt/**"]
depends_on = ["docs/ARCHITECTURE.md#threading", "docs/RULES.md#R-RT-001", "docs/RULES.md#R-RT-003"]
verified = { hash = "b3:92bff7b918b24cb1", by = "agent:claude@session_01SZXaqcgUA1wWCDzxs3hWJ6", note = "Dependency only (E1 changed ARCH threading): the new collector is per output for the render thread only; 'no collector thread yet' for the audio/mixer path and the InputControl keep-alive still hold" }
-->

Everything that runs on a real-time thread lives in `src/rt/`
([ARCH › Threads, clock and sync](../../docs/ARCHITECTURE.md#threading)). Each input's
**audio thread** (the platform backend's, one per stream) calls
`InputProducer::push_packet`. That call downmixes, tracks the device clock, resamples,
and writes into the input's ring. The **mixer thread** calls `Mixer::poll`, which reads
every ring for the block's window, applies gains, sums and meters. Neither allocates,
locks, blocks or logs after setup ([R-RT-001](../../docs/RULES.md#R-RT-001)). They
share only the rings and atomics: gains, meters and counters
([R-RT-003](../../docs/RULES.md#R-RT-003)).

**Ring** (`ring.rs`): a single-producer, single-consumer ring per input of stereo
frames, indexed by **engine frame** rather than queue position. The producer only
appends. It may jump forward, and the frames it skips read as silence; writing them costs
at most one ring's length. The mixer reads whatever window its clock asks for. Missing
frames are silence, counted as underrun. The producer never waits: it overwrites the
oldest frame, counting the ones the mixer hadn't read as overrun (drop oldest), and counts
frames that arrive behind the mixer's read position as late. `unsafe` is forbidden here
([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001)), so slots are `AtomicU32`s holding
`f32` bits. A seqlock-style check detects copies that raced with an overwrite: the
producer publishes the end of the write it is about to make before a release fence, and
the reader re-reads it after an acquire fence. No workspace crate provides a ring
indexed by time with these policies.

**Downmix** (`downmix.rs`), before resampling, so 5.1 costs no more than stereo:

| Layout | Left | Right |
|---|---|---|
| Mono `M` | `M` | `M` |
| Stereo | `L` | `R` |
| 5.1 `FL FR C LFE BL BR` | `FL + 0.7071·C + 0.7071·BL` | `FR + 0.7071·C + 0.7071·BR` |
| other | first channel | second channel (or the first) |

Mono is copied at full level, as OBS does. 5.1 uses the ITU-R BS.775 coefficients (LFE
dropped) without normalisation, so stereo content on a 5.1 endpoint passes unchanged.
Non-finite samples become 0, and samples are limited to ±64 (+36 dBFS) so no sum
overflows.

**Resampler** (`sinc.rs`): windowed-sinc interpolation at a fractional input position
that advances by `step = rate_in / rate_out · (1 + u)` per output frame.
- 128 taps (S3's kernel length), times `ceil(rate_in / rate_out)` when downsampling.
- A 4-term Blackman–Harris window.
- Cutoff at 0.94 of the lower Nyquist frequency.
- 256 precomputed phases with linear interpolation; every phase has unity DC gain.

The kernel is symmetric, so the resampler adds no delay, only `taps / 2` frames of
look-ahead. In the tests, a 1 kHz tone comes through within 0.00001 of its amplitude,
with everything else at −136 to −145 dB depending on the rates (the tests assert
−120 dB). A long silent gap is passed through as one run, without
computing each frame. S3 measured with rubato's asynchronous sinc resampler. This one
replaces it because it works frame by frame with no fixed input chunks (no staging
latency), its memory is fixed at construction, its output position is exact (the timing
error is computed from it), and it adds no dependency.

**Steering** (`input.rs`, `control.rs`, `drift.rs`, `continuity.rs`, ported from spike S3
and its measured tuning):
- **Continuity.** A device-position gap the clock agrees with (within 5 ms) is filled
  with silence. A backwards jump, a gap over 2 s, a gap the clock disagrees with, or a
  `discontinuity` flag (except on the first packet) makes the input re-anchor.
- **Drift fit.** A segmented least-squares fit of device position against stamp time.
  It rejects stamps more than 20 ms off the line (after 100 points); a run of 50 outliers
  restarts the segment. It also reports drift and jitter.
- **Anchoring.** An input is placed on the time line at its first accepted stamp, and
  again after a restart. A forward jump reads as silence; an overlap with audio already
  written is skipped and counted as late. Packets before the first stamp are dropped and
  counted.
- **Timing error.** After every packet, `e = next output frame / rate_out − T_in(next
  input position)`, with `T_in` extrapolated at the nominal rate from the last accepted
  stamp. `e` is smoothed (0.5 s) and drives a PI controller (0.02 Hz, damping 0.707,
  `u` clamped to ±5000 ppm, conditional-integration anti-windup) whose output `u` is
  the step correction. `u` settles at the device's drift. An error over 20 ms
  re-anchors instead.

Unlike S3, whose error was relative to its first measurement, this one is absolute, so
inputs line up with each other. Its sign is flipped because `u` scales the input step,
not rubato's output ratio.

**Mixer** (`mixer.rs`): per block, reads each ring, ramps each input's gain, sums,
ramps the bus gain, replaces any non-finite output with 0, and meters inputs (post-gain)
and the bus. Peaks use a `fetch_max` on the bits, which order like the values for
non-negative floats.

**Not yet enforced or measured:**
- No test proves the absence of allocation. R-RT-001 names
  `jstream_audio::rt::callback_does_not_allocate`, which needs `assert_no_alloc`, not yet
  a dependency. Until then, review enforces it.
- CPU cost is not measured ([R-RT-005](../../docs/RULES.md#R-RT-005)).
- Catching a panic at the thread boundary
  ([R-RT-006](../../docs/RULES.md#R-RT-006)) is the engine's job, where the threads are.
- There is no collector thread yet ([R-RT-003](../../docs/RULES.md#R-RT-003)). Instead,
  each `InputControl` holds a reference to its input's ring and counters, so as long as
  the control plane keeps one until the input's stream is closed and the mixer is
  dropped, the last reference is released on the control plane, never on the audio or
  mixer thread. The producer's and the mixer's own buffers are freed on their threads
  as those threads end. The collector hand-off comes with adding and removing inputs
  while running.

<a id="meters"></a>
## Meters
<!-- @section
covers = ["crates/jstream-audio/src/meter/**"]
depends_on = ["docs/adr/0028-audio-model-v2.md#decision"]
verified = { hash = "b3:f77ce7451ac1f6e0", by = "agent:claude@session_01MYuHTvXyZdeQGrKJhs9Htz", note = "New section, checked against src/meter/: filter length, cutoff and measured phase agreement, 100 ms steps and windows, gates, 0.02 LU histogram range and size, rate rule, and the tests listed; ADR-0028 decision (4x true peak, momentary/short-term/integrated, allocation-free, reset action) matches" }
-->

`src/meter/` holds the broadcast meters
[ADR-0028](../../docs/adr/0028-audio-model-v2.md) asks for, as pure DSP over interleaved
stereo blocks. They will run on the mixer thread, so each builds its fixed-size state in
its constructor and never allocates, locks or blocks after that
([R-RT-001](../../docs/RULES.md#R-RT-001)). They aren't wired into the mixer, its
`MeterReading`s or any protocol yet ([Not yet](#follow-ups)).

**`TruePeakMeter`** (ITU-R BS.1770 Annex 2): each channel is upsampled 4× and the largest
absolute value is held until `take_peak`, as the sample-peak meters are. A reading is
never below the sample peak.
- The annex's 48-tap filter is only an example. Its phases differ in treble gain, so it
  reads a 12 kHz tone at 48 kHz up to +0.22 dB high at some phases, just outside EBU
  Tech 3341's +0.2 / −0.4 dB tolerance, and it rejects images by only about 40 dB.
- This meter uses a 192-tap Blackman–Harris windowed sinc instead, built like the
  resampler's, with a cutoff at 23/48 of the input rate and each phase at unity DC gain.
  At 48 kHz its phases agree within 0.02 dB up to 20 kHz, and images are more than
  110 dB down.
- 4× oversampling still misses a peak between two upsampled points by up to
  `cos(π·f / (4·rate))`: −0.27 dB at 15 kHz, −0.48 dB at 20 kHz.

**`LoudnessMeter`** (BS.1770 K-weighting and gating, stereo, `G = 1` per channel):
- Squares are summed per 100 ms step, and the last 30 steps are kept. Momentary is the
  last 400 ms and short-term the last 3 s, both updated every 100 ms. Each reads `None`
  until its window has filled once.
- Each momentary value is also a gating block (400 ms, 75 % overlap). Integrated
  loudness applies the −70 LUFS absolute gate and the −10 LU relative gate.
- So that memory stays fixed, blocks are counted in 0.02 LU bins from −70 to +30 LUFS
  (80 KB), each bin holding its count and the exact sum of its blocks' energy. The only
  approximation is that the relative gate falls on a bin edge.
- `reset_integrated` restarts the integrated measure, for ADR-0028's
  `audio.reset_loudness`.
- The K-weighting coefficients are derived for any rate that is a whole number of frames
  per 100 ms, and they reproduce BS.1770's 48 kHz tables.

**Tests** (`src/meter/`), against reference values:
- K-weighting: BS.1770's 48 kHz coefficient tables, and the same response at 44.1, 48
  and 96 kHz.
- Loudness: EBU Tech 3341 cases 1 to 5, synthesised (1 kHz stereo tones, which also
  exercise both gates), at ±0.1 LU. Also the window lengths, block-size independence,
  channel summing, resets and extreme input.
- True peak: tones up to 15 kHz at 16 phases each, within EBU Tech 3341's tolerance. A
  quarter-rate tone between its samples reads +3 dBTP from 0 dBFS samples. Also
  20 kHz against the 4× bound, block-size independence and the sample-peak floor.

The EBU's own test files and the real-program cases (Tech 3341 cases 6 to 8, and the
true-peak cases from files) are not run; they would add binary fixtures.

<a id="filters"></a>
## Filters
<!-- @section
covers = ["crates/jstream-audio/src/filter/**"]
depends_on = ["docs/adr/0028-audio-model-v2.md#decision"]
verified = { hash = "b3:ba0f2869af941025", by = "agent:claude@session_01L5hE1i9KRciNcKrq7DGnVM", note = "New section, checked against src/filter/: trait methods, each kernel's ranges, defaults, detector, ramps, latency (limiter look-ahead, delay 0 as a deliberate shift per ADR-0028 D15), limiter algorithm and clamp, tests listed; ADR-0028 decision (keys, latency reporting, delay ring allocated at max on construction, allocation-free) matches" }
-->

`src/filter/` holds the audio filter kernels
[ADR-0028](../../docs/adr/0028-audio-model-v2.md) puts in source and bus filter chains,
as pure DSP over interleaved stereo blocks at the engine rate, processed in place. They
will run on the mixer thread, so each builds its fixed-size state in its constructor
(the limiter's look-ahead and the delay's ring at their maximum) and never allocates,
locks or blocks after that ([R-RT-001](../../docs/RULES.md#R-RT-001)). Settings are
plain structs, range-checked by `validate` and by the constructors and `set_params`;
how they reach the mixer thread (snapshots or parameter channels) comes with the
wiring. Non-finite input samples are treated as 0.

**`AudioFilter`** is the trait a chain holds: `process` a block, `process_keyed` with a
read-only key block for the same frames, `wants_key` (so the mixer processes the key's
source first), `latency_frames` and `reset`.

| Kernel | What it does | Latency |
|---|---|---|
| `Gain` | −120 to +24 dB or `-inf`; a change ramps linearly over 10 ms | 0 |
| `NoiseGate` | Open and close thresholds (hysteresis), hold, linear attack and release ramps, a range (gain when closed). Detector: the larger channel's level, instant attack, 10 ms decay. Defaults are OBS's | 0 |
| `Compressor` | Threshold, ratio (up to `inf`), quadratic soft knee, makeup; gain reduction smoothed in dB with attack and release time constants (Giannoulis, Massberg and Reiss, 2012). With a key, it detects on the key instead: ADR-0028's ducking | 0 |
| `Limiter` | Look-ahead (0 to 20 ms, fixed when built) sample-peak limiter: sliding minimum of the needed gain over the look-ahead, release smoothing, then a box average over the look-ahead, so the gain ramps down before a peak and the output never exceeds the ceiling | the look-ahead |
| `Delay` | Up to a maximum fixed when built (at most 5000 ms) | 0: a deliberate shift, which the mixer stamps later instead of compensating |

Detectors are stereo-linked (one gain for both channels). The limiter's last step clamps
at the ceiling, which only catches rounding, or audio already in its delay line when
the ceiling is lowered; the tests check the gain alone holds it. It limits sample
peaks, not true peaks.

**Tests** (`src/filter/`), with reference signals (square waves, whose level a peak
detector reads as steady, sines and single-frame peaks): the gain's level and ramp;
the gate's thresholds, hysteresis, hold, attack, release and range; the compressor's
static curve, soft knee, attack and release time constants, and ducking from a key;
the limiter's latency, ceiling under bursts, transients and +12 dB overload at several
look-aheads, its ramp ahead of a peak and its recovery; the delay's shift, maximum and
changes; block-size independence and reset where state is kept; rejected settings.

**Not yet:** running them in the mixer (filter chains on sources and buses, keys and
latency compensation in the audio graph), their settings in kind descriptors and the
protocol, a no-allocation test ([Real-time design](#realtime)), and a CPU measurement:
their cost is not measured ([R-RT-005](../../docs/RULES.md#R-RT-005)).

<a id="follow-ups"></a>
## Not yet
<!-- @section
-->

- **Input format before the first packet.** `AudioIo::open_input` takes the handler
  before it reports the stream's format (`AudioStream::format`), but `add_input` needs
  the format to build the producer. Until the platform API reports it up front (for
  example, a format query per target, or the format in the first callback), the engine
  must know the device format before opening the stream.
- **Adding and removing inputs while the mixer runs,** through a lock-free command queue,
  with dropped rings going to the collector thread.
- **State snapshots** ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading)). The
  mixer's live state is atomics for the gains plus an input list fixed when it is built.
  That fits ARCH's "continuous parameters bypass snapshots": a fader move is a store to
  an atomic, and nothing structural changes while the mixer runs. The `arc-swap`
  snapshot ARCH gives each subsystem comes with adding and removing inputs live (above),
  when the input list starts to change.
- **A panic in `on_packet` or `Mixer::poll`**
  ([R-RT-006](../../docs/RULES.md#R-RT-006)). Nothing in this crate catches one or
  mutes the bus. The test R-RT-006 names, `scenarios::rt_panic_is_reported`, doesn't
  exist yet, so no test covers the audio path. Until it does, two thread boundaries do:
  `InputProducer::on_packet` runs on the platform's audio thread, whose `catch_unwind`
  (`guarded` in `jstream-platform-windows`) stops that stream and reports the panic to
  the handler's `on_error` as `InvalidState`; and `Mixer::poll` runs on the engine
  thread that calls it, which must catch a panic there, mute the bus and raise the
  error event. `rt_panic_is_reported` lands with that engine thread.
- **The rest of the audio model** ([ADR-0028](../../docs/adr/0028-audio-model-v2.md)):
  the activity gain per source, the send matrix and further buses, filter chains with
  keys and latency compensation ([Filters](#filters) has the kernels), running the true-peak and loudness meters in the mixer (for the
  master bus and subscribed buses and sources) with their meter-stream fields and
  `audio.reset_loudness`, and monitor taps; and render-side resampling for output
  devices.
- **A no-allocation test** with `assert_no_alloc`, and a CPU measurement on hardware.
- **Feeding the fitted drift forward** into the controller, which would shorten the
  start-up transient (about 0.46 · drift / (2π · 0.02 Hz): 0.55 ms at 150 ppm).
