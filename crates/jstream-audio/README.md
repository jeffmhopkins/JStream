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
-->

The engine's real-time audio mixer. Layer 2: no I/O and no platform code. It depends
on [`jstream-media`](../jstream-media/README.md) for formats and timestamps, and on
[`jstream-platform`](../jstream-platform/README.md) only for the audio input handler
it implements. This first cut covers what [ROADMAP M1a](../../ROADMAP.md#m1) needs
(a WASAPI microphone and desktop loopback mixed for the AAC encoder): adaptive
resampling of each input onto the engine time line, and metering. It also has
[ADR-0028](../../docs/adr/0028-audio-model-v2.md)'s routing: each input is a source's
audio stream with its filter chain, fader and mute, and feeds stereo buses through a
send matrix with a gain per send, applied after the fader. The master bus always
exists, and every new input sends to it at 0 dB. Each bus has its own filter chain,
fader and mute. Chains run the kernels in [Filters](#filters), and the mixer
compensates their latency. It also has
[ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)'s audio half: a
master chain on every bus that feeds an output, output taps with their own chains for
outputs that encode their own audio, and each bus's output before the master chain for
monitoring. Each input has an activity gain (audio that
follows the program) before its sends. A bus monitor (ADR-0028's D22) hears the bus before or after its filters and
plays on an output device through a monitor player; `jstream-core` opens one for each
bus monitor in the collection (protocol 0.42) while the program stage runs. A
compressor in any chain can take another input as its sidechain key, for ducking
(ADR-0028); the mixer runs keys first. The true-peak and loudness meters
([Meters](#meters)) exist as DSP but don't run in the mixer yet; `jstream-core`'s
mixer thread runs them on the master block the mixer returns.

**Time line.** Engine frame `n` is media time `n / rate` on the media clock
([ADR-0014](../../docs/adr/0014-media-clock.md)), with `rate` the engine rate (48 kHz by
default). Each input is resampled so that the audio it captured at media time `t` lands on
engine frame `t · rate`. Inputs therefore line up with each other and with anything else
stamped on the clock, and no device is the master.

**API.** Each entry point's contract (ranges, limits, threads, what allocates) is in its
rustdoc: `cargo doc -p jstream-audio --open`. In outline:

| Entry point | What it does |
|---|---|
| `Mixer`, `MixerConfig` | the mixer: engine rate, block size (1024 frames), latency (30 ms) and ring size; `poll(now, out)` mixes the next block once the clock has passed it, `next_due` says when that is, `mix` mixes a given block |
| `MixerControl` | adds inputs, buses, output taps and monitors from another thread while `poll` runs, through a command queue the mixer applies without allocating |
| `InputProducer` | a device stream's `AudioInputHandler`, for `AudioIo::open_input` |
| `InputControl`, `BusControl` | fader, mute, sends (inputs into buses, post-fader), activity gain, filter chains, meters and `InputStats` / `BusStats` |
| `MasterControl`, `OutputTapControl` | the master chain on every bus that feeds an output, and an output's own chain |
| `FilterSpec`, `FilterControl` | a chain's filters (gain, gate, compressor, keyed compressor, limiter, delay, or a CLAP filter's shared block, `insert_host_filter`) |
| `Mixer::bus_block`, `input_block`, `monitor_block`, `output_block` | the last block of a bus, an input, a monitor tap or an output tap |
| `Monitor`, `MonitorSocket`, `MonitorPlug` | a bus monitor played on an output device |
| `meter::TruePeakMeter`, `meter::LoudnessMeter` | the true-peak and BS.1770 loudness meters ([Meters](#meters)) |
| `filter::AudioFilter` and its kernels | the DSP behind each filter ([Filters](#filters)) |

Limits: 64 inputs, 16 buses (`MAX_BUSES`, the master included), 16 output taps, and
8 filters and 100 ms of latency per chain. `Mixer::latency_frames` is the chains'
latency the mixer compensates, and each block's `pts` is its first frame minus it.
Errors are `AudioError`, none of whose variants allocates.

**Tests.** The unit tests in each `src/rt/`, `src/meter/` and `src/filter/` module cover
that module's behaviour, including the threads (a two-thread ring test, inputs and buses
added from another thread, frees on the collector thread).
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

`tests/no_alloc.rs` is R-RT-001's `callback_does_not_allocate`: the real-time calls
under `assert_no_alloc`, through the same kinds of glitches and live input, bus, send,
filter, master-chain, output-tap and CLAP-filter changes,
reading each output tap's and each input's block after every block, feeding two
monitors (before and after the master's filters) and rendering their devices every
tick through the sockets their players are plugged into
([Real-time design](#realtime)).
`tests/cost.rs` is an ignored measurement of what a block costs
([Real-time design](#realtime)).

The crate is built with `opt-level = 3` in the dev profile (root `Cargo.toml`). Debug
builds of the app need a resampler fast enough for a real-time thread, and the
simulations need it to finish in seconds.

<a id="realtime"></a>
## Real-time design
<!-- @section
covers = ["crates/jstream-audio/src/rt/**"]
depends_on = ["docs/ARCHITECTURE.md#threading", "docs/RULES.md#R-RT-001", "docs/RULES.md#R-RT-003"]
-->

Everything that runs on a real-time thread lives in `src/rt/`
([ARCH › Threads, clock and sync](../../docs/ARCHITECTURE.md#threading)). Each input's
**audio thread** (the platform backend's, one per stream) calls
`InputProducer::push_packet`. That call downmixes, tracks the device clock, resamples,
and writes into the input's ring. The **mixer thread** calls `Mixer::poll`, which reads
every ring for the block's window, applies faders, activity gains and sends, sums each
bus and meters. Neither allocates,
locks, blocks or logs after setup ([R-RT-001](../../docs/RULES.md#R-RT-001)). They
share only the rings and atomics: gains, sends, meters and counters
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

**Monitors** (`monitor.rs`). The mixer thread writes each monitored block into a ring
of the same kind, under its engine frames. The ring is sized for the monitor, not from
the inputs' `ring_frames`: twice the sum of the target, a block and the 50 ms threshold,
rounded up to a power of two, so a short mixer ring can't push a steady read past the
ring's half-capacity guard. The output device's audio thread reads
it through `MonitorPlayer::render`: in 64-frame stages, through a resampler from the
engine rate to the device's, steered by the inputs' controller and smoothing so that it
reads a fixed distance behind the newest frame written. That target is 1.5 blocks plus
20 ms (2496 frames, 52 ms, at the default 1024-frame block): the distance swings by a block as the mixer writes
them, and the rest covers a device period and the look-ahead. More than 50 ms above the
target (the device stalled) moves the read position forward onto it, counted as a
re-anchor. More than 50 ms below (the feed stalled or stopped) leaves the player
reading on into silence, counted as underrun, with the controller held: moving back
would replay what was already heard. Both ends hand the ring and counters to the
collector when dropped. The device stream opens on a `MonitorSocket` before the player
exists; the player, built on the control thread once the stream's format is known,
reaches it through a one-slot `crossbeam` channel, which the socket polls with
`try_recv` (no lock, no wait) until it arrives.

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
  restarts the segment. The first stamp after a `discontinuity` flag is never rejected:
  off the line, it restarts the segment at once. It also reports drift and jitter.
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

**Mixer** (`mixer.rs`): per block, reads each ring, runs each input's filter chain and
compensation delay, ramps its fader and meters it, copies it into the input's own
block with its activity gain ramped (`input_block`; the block is allocated with the
input on the control plane, travels in its add command and goes to the collector with
it), then adds it into every bus it sends to, ramping each send's gain. For each bus it
then copies the sum into a third buffer (the `PreFilters` monitor block), runs the bus
chain and compensation delay, ramps the bus fader on both, replaces any non-finite
sample with 0 and meters the output.
Each bus that feeds an output then runs its copy of the master chain on a second
buffer, so the first keeps the pre-master output for monitoring (`bus_block`), and each
output tap copies its bus's post-master block and runs its own chain. The master's
post-master output is the block `poll` returns. Peaks use a `fetch_max` on the bits,
which order like the values for non-negative floats.

**Buses and sends** (`bus.rs`). The mixer holds 16 bus slots, each with a mix buffer
allocated in `Mixer::new`; slot 0 is the master. Each input has one send per slot (an
atomic bus id and an atomic gain), so the send matrix is fixed-size and a send change
is two atomic stores: the gain first, then the target, with release ordering so the
mixer never applies a stale gain to a new target (a retarget only happens once the old
bus is gone from the slot). A send counts only while its slot
holds the bus it names. Bus ids are never reused, so a send to a removed bus stays
silent even after another bus takes the slot. Each `ControlPlane` takes a tag from a
process-wide counter when its mixer is made, and stamps it on its `BusId`s; the
control handles check it before touching a send or adding an output tap, so another
mixer's bus id never reaches the matrix or a tap (the master's id carries tag 0, valid for any mixer). The mixer remembers which bus each send's
ramp was for, and starts again from silence when that changes. Skipping silent sends
keeps the cost at one pass over the block per active send. Sends are post-fader only,
as ADR-0028 decides.

**Filter chains** (`chain.rs`). Every input and bus has a chain of up to 8 filters,
reserved when the input or bus is built on the control thread, so the mixer inserts and
unlinks filters without allocating. The control plane builds each filter (its delay
lines and look-ahead at their maximum) and sends it through the command queue; a removed
filter, and a removed input's or bus's chain, goes to the collector. An edit is a plain
value the mixer applies with the kernel's own setter, after the control plane has
checked its kind and range. The control plane keeps a record of each chain's filters
for that check, so it never reads the mixer's copy. Inserting or removing a filter
takes effect at a block edge without a crossfade.

**Sidechain keys** (ADR-0028). A keyed compressor names the input whose block drives
its detector. After applying a block's commands, when any command changed something, the
mixer gives each input a depth one more than its deepest key's, relaxed in at most one
pass per input, and sorts the inputs by it in place (`sort_unstable_by_key`, no
allocation), so every key runs before the inputs keyed on it. A cycle leaves its
inputs at the cap and reads as silence to whichever runs first: the engine refuses
cycles, so this only keeps a bad edit from mattering. An input's chain reads its key's
`input_block` from the inputs already run this block, and a bus chain from any input.
A key that is gone, silenced as a key (`key_silenced`) or not yet run is an empty
block, which the compressor reads as silence. ADR-0028 sorts the audio graph on the
control thread when the audio snapshot is built; with no audio snapshot yet
([Not yet](#follow-ups)), the mixer sorts on its own thread instead, bounded and
without allocating, as it recomputes latency in `apply_commands`. The key block is the
key's output after its latency-compensation delay, while an input's chain runs before
its own, so when an input chain has latency (a limiter's look-ahead) the key reaches
the detector up to that latency late, and the ducking lags by as much; a bus chain's
key is aligned.

**Latency compensation.** Each chain's latency is the sum of its filters' (the
limiters' look-ahead; a `Delay` filter is a deliberate shift and counts 0, per
ADR-0028). After its chain, every input is delayed to the longest input chain's latency
and every bus to the longest bus chain's, by a compensation delay each chain allocates
for 100 ms when built, so every bus sum lines up and buses line up with each other.
Each block's `pts` is its first frame minus the two maxima: the media time of the input
it carries. A `Delay` adds nothing to that, so input audio from media time `t` lands
in the block stamped `t` plus the delay: ADR-0028's "stamped later by its delay". Adding
or removing a filter with latency changes the delays at once (the
frames in between are skipped or repeated) and moves `pts` by the difference, so a
consumer sees a gap or an overlap there. The master chain has the same filters on
every bus, so its latency needs no compensation: it is subtracted from `pts` too. An
output tap's latency is subtracted only from that tap's `pts`, since each tap feeds its
own encode.

**CLAP filters** (`host.rs`,
[ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)). A CLAP plugin runs in
its own host process; its filter in a chain is the mixer's side of the shared block
the two use ([`jstream-plugin-proto`](../jstream-plugin-proto/README.md)). Each block
it writes its dry input into the input ring as block `n` and reads the host's block
`n − depth` from the output ring, with plain atomic loads and stores: no syscall, lock
or wait. Its latency, fixed when it is built, is `depth` blocks plus the plugin's own
reported latency, so the chain compensates it like any filter's. When the host's block
isn't there (late, hung or dead), the filter passes its own dry signal delayed by that
same latency, so timing doesn't jump, and counts the miss for the control thread to
report: `MixerMisses`' `total`, and `run` for misses in a row. They are in the
engine's memory beside the mapping (`EngineBlock`), not in the shared block, so a host
can't reset them to hide a degraded filter (#351). Switching between the two is a crossfade over 2 ms (at most one block): dry to
the plugin's output when it comes back, and from the plugin's last frame to dry when it
goes missing, since the missing block can't be faded. The host's samples are another
process's data, so non-finite ones become 0. A chain reset clears the dry delay and
bumps `reset_seq` for the host to reset the plugin; block numbers carry on, so the
rings stay in step. A filter numbers its blocks on from the input ring's head, so when
a chain is rebuilt a new filter can go on a block whose filter was removed and the host
carries on without a restart. The head is read when the new filter is built, after the
old one's removal is queued but maybe before the mixer applies it, so a block the old
filter writes in between can be written again under the same number: a one-block
glitch, only on a rebuild, which resets the built-in filters anyway. The shared block (`HostMemory`) is dropped on the collector like
any removed filter.

**Master chain and output taps** (`tap.rs`,
[ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)). The master chain
runs once per block on each bus that feeds an output: the master bus, and each bus an
output tap reads. Each such bus has its own copy, so a limiter on the master never
mixes two buses' state. The control plane builds one copy of a new master filter per
bus and sends them in one command, so every copy starts on the same block; an edit or a
removal reaches every copy the same way. It keeps each master filter's spec with its
edits applied, so a bus added later is built with the chain as it stands. A bus that
stops feeding outputs skips its copy, and the copy restarts from silence when the bus
feeds one again. An output tap (at most 16, reserved in `Mixer::new`) is built on the
control thread with its block buffer and its own chain; it reads silence once its bus
is removed. Removed taps and master copies go to the collector. A CLAP filter on the
master chain runs one host per bus that feeds an output
([ADR-0086](../../docs/adr/0086-master-clap-filter-one-host-per-output-bus.md)), since
a host takes one chain's blocks
([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md) › 1). The master bus's
copy runs on the host the filter was added with. Every other bus's copy, one added
later included, starts as a stand-in that delays its dry signal by the filter's
latency, so the master chain's latency stays the same on every bus, until
`FilterControl::set_bus_host` gives it a host of its own. That builds the copy's new
kernel on the control thread, refuses a host that isn't ready or whose latency differs
from the filter's, and queues it; the mixer swaps it in place at the start of a block,
with no allocation, and hands the replaced kernel to the collector. With no host it
goes back to the stand-in. The engine gives a bus its hosts while an output taps it
(`jstream-core`'s routing).

**Adding and removing inputs and buses** (`cmd.rs`, `retire.rs`). The control thread
allocates the ring and the producer (or a bus's shared state) and sends them on a
bounded lock-free queue (`crossbeam-channel`, 128 commands). The mixer applies that
queue at the start of the next block. Its input
list is reserved for 64 inputs, so installing one does not allocate, and removing one
unlinks it. A bus goes into a free slot the control plane picked; removing it (never the
master) empties the slot and hands its shared state to the collector. The mixer and the
producer each hold an `Arc` of the ring. Releasing either
reference hands that `Arc` to the collector thread, which is where the ring is freed
([R-RT-003](../../docs/RULES.md#R-RT-003)), including when the producer is dropped on
the audio thread after the mixer is already gone. Each mixer starts its own collector in
`Mixer::new` and hands it to its control plane and producers, so nothing is global
([R-RS-008](../../docs/RULES.md#R-RS-008)). It ends once the mixer, every producer and
its control handles are gone. It polls its queue every 10 ms rather than blocking on it,
so a hand-off is a `try_send` that never wakes a thread or takes the channel's lock.
Each `InputControl` also holds a reference to its input's ring and counters, so when the
control plane drops the last one it frees them there, also off the real-time threads. A
full command queue refuses the add or the removal rather than blocking the mixer.

**No allocation, tested** (`tests/no_alloc.rs`). The test R-RT-001 names,
`rt::callback_does_not_allocate`, runs in its own test binary with `assert_no_alloc`'s
allocator installed globally. After 15 s of warm-up, it runs 30 s of simulated 10 ms
ticks. In each tick, every input's `on_packet`, `Mixer::poll` (with its filter chains)
and a keyed compressor and the meters on each mixed block run inside an
`assert_no_alloc` scope. Any allocation or free
there fails the test, in debug and release builds alike. Five inputs (mono 44.1 kHz,
stereo 48 and 96 kHz, 5.1, and 32 kHz added while running) drift and jitter. The checked phase also
covers timestamp outliers and missing stamps, lost packets, a discontinuity (and one
whose stamp is off the clock line, which restarts the drift fit's segment), a stalled
audio thread that catches up, a 3 s device gap that resets the mapping, silent and
non-finite packets, a mixer stall long enough to overrun the rings and skip frames,
an input added and one removed while the mixer runs, a bus added, sent to, given a
limiter and removed, filters on an input and the master edited every 0.5 s, and a
limiter added to and removed from an input (so the compensation delays move), master
filters edited every 0.5 s and one added to both fed buses and removed, an output tap
on the aux bus with its own chain (edited every 0.5 s, outliving its bus) whose block is
read after each mix, a tap on the master added and removed, and every input's own
block read after each mix. The test
checks that each of those paths ran. Control-plane calls and dropping a producer (stream teardown) run
outside the scope. Locks, blocking and logging are still left to review.

**Cost** (`tests/cost.rs`, [R-RT-005](../../docs/RULES.md#R-RT-005)). An ignored test
times `Mixer::mix` alone (no resampling) for 20 s of 1024-frame blocks (21.3 ms each)
with 8 stereo 48 kHz inputs. Measured on a cloud container (4 vCPUs of an Intel Xeon at
2.80 GHz, Linux 6.18), release build, mean per block:
- master bus only: 56 µs (0.26% of the block period);
- 8 buses, every input sending to every bus: 105 µs (0.49%);
- the same, plus a gate and a compressor per input, a compressor and a 5 ms limiter per
  bus, a gain and 3 ms limiter master chain on all 8 buses, and 7 output taps each with
  a gain and 5 ms limiter (62 filters): 957 µs (4.5%; p99 1.6 ms).

Keeping each input's own block (`input_block`) costs one copy and gain pass per input
per block: measured on a cloud container (4 vCPUs of an Intel Xeon at 2.10 GHz, Linux
6.18), release build, the master-only case went from 47 to 54 µs mean, and the other
two moved within their run-to-run noise.

Run it on the target machine with
`cargo test --release -p jstream-audio --test cost -- --ignored --nocapture`.

**Benchmarks** (`benches/mix.rs`, criterion). `mix_block` times the same three mixer
scenarios per block, and `input_push` times one engine block's worth of device frames (1024 at 48 kHz, 940 or 941
at 44.1 kHz) through `InputProducer::push_packet` (downmix and adaptive resampling).
Measured on a cloud container (4 vCPUs of an Intel Xeon at 2.10 GHz, Linux 6.18), release
build, mean: `input_push` about 200 µs at either rate (0.9% of the block period). The
`bench` workflow records them on every push to `main`
([R-GIT-003](../../docs/RULES.md#R-GIT-003)). Run them with
`cargo bench -p jstream-audio --bench mix`.

**Not yet enforced or measured:**
- The cost on Windows hardware is not measured.
- Catching a panic at the thread boundary
  ([R-RT-006](../../docs/RULES.md#R-RT-006)) is the engine's job, where the threads are.

<a id="meters"></a>
## Meters
<!-- @section
covers = ["crates/jstream-audio/src/meter/**"]
depends_on = ["docs/adr/0028-audio-model-v2.md#decision"]
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
| `Delay` | Up to a maximum fixed when built (at most 5000 ms) | 0: a deliberate shift, which the mixer doesn't compensate, so its output is stamped later by its delay |

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
The mixer runs them in each input's and bus's chain, with their latency compensated
([Real-time design](#realtime)). `tests/no_alloc.rs` runs all five under the
allocation check: the gate and compressor on an input, gain, delay and limiter on the
master bus's own chain, a limiter on another bus and input, gain, limiter and
compressor in the master chain and an output tap's chain, keyed compressors on two
inputs (a chain of keys three inputs long), and a keyed compressor on the mixed block.

The master chain and output taps run the same kernels.

The kernels' cost in the mixer is measured in
[Real-time design](#realtime) ([R-RT-005](../../docs/RULES.md#R-RT-005)).

<a id="follow-ups"></a>
## Not yet
<!-- @section
-->

- **State snapshots** ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading)). Gains
  stay atomics, which is ARCH's "continuous parameters bypass snapshots". The input
  list changes through the command queue in [Real-time design](#realtime), not through
  an `arc-swap` snapshot, and so do buses; sends are atomics per input and bus slot.
  Filters change through the same queue. The snapshot ARCH gives each subsystem, for
  the rest of the audio graph, is still later; the order keys impose is kept by the
  mixer itself (*Sidechain keys* in [Real-time design](#realtime)).
- **A panic in `on_packet` or `Mixer::poll`**
  ([R-RT-006](../../docs/RULES.md#R-RT-006)). Nothing in this crate catches one: the
  thread boundaries that call it do. `InputProducer::on_packet` runs on the platform's
  audio thread, whose `catch_unwind` (`guarded` in `jstream-platform-windows`) stops
  that stream and reports the panic to the handler's `on_error` as `InvalidState`.
  `Mixer::poll` runs on `jstream-core`'s mixer thread, whose `catch_unwind` wraps the
  whole mix loop: a panic there is fatal, so the output settles `failed` (the error
  event) and the pipeline is taken down, which silences every bus with the mixer.
  `scenarios::rt_panic_is_reported` (in `jstream-core`) panics on the mixer thread as
  it calls `Mixer::poll` and checks that both clients see `failed` and the engine
  still runs a command. No test panics inside `Mixer::poll`'s body: it runs only this
  crate's own filters, so nothing a test supplies can.
- **The rest of the audio model** ([ADR-0028](../../docs/adr/0028-audio-model-v2.md)):
  running the true-peak and
  loudness meters in the mixer (for the
  master bus and subscribed buses and sources) with their meter-stream fields and
  `audio.reset_loudness`, and monitor devices that play `bus_block`; and render-side
  resampling for output devices.
- **The engine side of CLAP filters**
  ([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)): the mixer's side is
  here (`insert_host_filter`), and starting and watching the host process are built
  (`jstream-platform`'s `PluginHosts`, `jstream-core`'s `HostSupervisor`), and
  `jstream-core` runs a CLAP filter from a source's, the master's or an output's chain
  and sends its settings as
  parameter changes, and the miss counts are kept on the engine's side of the block
  (#351), but reporting them in `stats.tick` and the degraded event in the protocol
  are not.
- **A CPU measurement** of the mixer on Windows hardware, and of resampling
  ([Real-time design](#realtime) has a cloud machine's numbers for the mix).
- **Feeding the fitted drift forward** into the controller, which would shorten the
  start-up transient (about 0.46 · drift / (2π · 0.02 Hz): 0.55 ms at 150 ppm).
