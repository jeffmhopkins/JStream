<!-- @doc
id = "CRATE-jstream-aac"
title = "jstream-aac"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-aac

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-aac/src/**", "crates/jstream-aac/tests/**", "crates/jstream-aac/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-aac", "docs/adr/0061-source-types-and-decoding-scope-v2.md", "docs/adr/0062-aac-lc-encoder-patent-check.md", "docs/RULES.md#R-RT-002", "docs/RULES.md#R-DEP-001"]
-->

JStream's own AAC-LC encoder, the one Linux streams with
([ADR-0061](../../docs/adr/0061-source-types-and-decoding-scope-v2.md) › AAC on Linux).
Windows and macOS keep their operating systems' encoders. Layer 0: safe Rust with no C,
no `unsafe` and no I/O, and `thiserror` its only dependency. It encodes only: there is no
decoder. `jstream-platform-linux` puts it behind the `AudioEncoder` trait.

**Licensing.** JStream holds no AAC patent licence. The AAC patent pool is administered
by [Via Licensing Alliance (Via LA)](https://www.via-la.com/licensing-programs/aac/).
[ADR-0062](../../docs/adr/0062-aac-lc-encoder-patent-check.md) records what the patent
check found and what it could not confirm. Nothing here says AAC-LC is free of patents.

**ADR-0062's limits**, which bind every change to this crate:
- **Tools.** The stream is the MPEG-4 AAC-LC object type (2), using only tools of the
  MPEG-2 AAC LC profile (ISO/IEC 13818-7, 1997). This crate uses the filterbank with sine
  and KBD windows, block switching, temporal noise shaping (TNS), M/S stereo,
  scalefactors, quantization and Huffman coding. Intensity stereo is allowed and not built
  yet. PNS, SBR, parametric
  stereo and every later tool are never emitted. Adding one needs a new maintainer-tier
  ADR.
- **Techniques.** Each technique cites its pre-2003 source in the module that implements
  it. The psychoacoustic model is Johnston (1988) with Zwicker and Terhardt's Bark scale,
  Schroeder et al.'s spreading function and Terhardt's threshold in quiet, plus 13818-7
  Annex B's pre-echo control and perceptual entropy (`psy`). The attack detector follows
  Edler (1989), and the window-sequence state machine is the standard's (`psy`,
  `encoder`). Quantization and the two nested loops are Annex B's (`quant`), and so are
  the bit reservoir and sectioning (`encoder`, `huffman`). M/S is decided per band after
  Johnston and Ferreira (1992). The MDCT uses the fast algorithm of Duhamel, Mahieux and
  Petit (1991) (`mdct`). TNS follows Herre and Johnston (1996), with Levinson-Durbin,
  Tohkura et al.'s lag window (1978) and Markel and Gray's white-noise correction (1976)
  (`tns`).
- **No decoder ships.** The tests decode with Symphonia's pure-Rust AAC decoder
  (`symphonia-codec-aac`, MPL-2.0, allowed by [R-DEP-001](../../docs/RULES.md#R-DEP-001)),
  a dev-dependency only.

**What it does.** `Encoder::new(Config)` takes 44.1 or 48 kHz, mono or stereo, and a
target of 16 to 256 kbps per channel. `push_interleaved` or `push_planar` queue `f32`
samples (clamped to full scale; NaN is silence), and `encode_frame` appends the next raw
access unit (a `raw_data_block` with one SCE or CPE, no ADTS header) to a buffer. Each
unit covers `FRAME_LEN` (1024) samples. `finish` pads the last frame with silence and adds
one unit so the decoder outputs every sample. The decoded output lags the input by
`DELAY` (1024) samples, the usual AAC priming. `audio_specific_config` gives the two-byte
`AudioSpecificConfig` (`11 90` for 48 kHz stereo), the same bytes `jstream-mux` writes for
FLV and MP4.
- **Filterbank.** A 2048-line MDCT for long windows and eight 256-line ones for short
  windows, with sine or KBD windows (`Config::window_shape`, sine by default). An attack
  in the samples a frame's short windows would cover switches that frame to
  `EIGHT_SHORT`, through `LONG_START` and `LONG_STOP`, with 576 samples of lookahead.
  Short windows aren't grouped yet.
- **Model.** Per scalefactor band: energy, a masking threshold (spreading, a tonality
  offset from spectral flatness, and the threshold in quiet as a floor), and its
  perceptual entropy. The coded bandwidth rises with the bit rate per channel, from about
  16 kHz at 64 kbps to 20 kHz.
- **TNS.** Per window, an LPC analysis across frequency over the lines from about 1.4 kHz
  to the standard's TNS band limit (40 long bands at 48 kHz, 42 at 44.1 kHz, 14 short),
  of order 12 for long windows and 7 for short. The bands' masking thresholds are added
  to the zero lag, so a steady tone's window leakage doesn't count. A filter is sent when
  the prediction gain clears 1.4, with its reflection coefficients quantized at 4-bit
  resolution (3 bits when they fit, `coef_compress`) and trailing zeros trimmed. The lines
  are filtered with the dequantized coefficients, so the decoder's synthesis filter
  inverts it exactly, and the filtered bands' thresholds are divided by the gain, since
  that filter raises the quantization noise by about as much. Each channel is filtered
  before the M/S decision, and its `tns_data()` bits come out of its budget.
  `Config::tns` turns it off, for comparison.
- **Stereo.** A CPE with a common window. Each band is M/S or L/R, whichever has the lower
  perceptual entropy, with the M and S thresholds at half the smaller of L's and R's.
- **Quantization.** The inner loop bisects the global step to meet the channel's budget,
  and the outer loop amplifies the bands whose noise exceeds their threshold. The best
  result is kept. Scalefactors stay within the codebook's ±60 range, and quantized values
  within the escape codebook's 8191.
- **Rate.** A bit reservoir: each frame earns the mean (`bit_rate * 1024 / sample_rate`),
  and may spend what the reservoir holds, scaled by its perceptual entropy against the
  running mean. A unit never exceeds the decoder's 6144-bit buffer per channel, and the
  average never exceeds the target. Bits a full reservoir can't hold are spent down, but
  on very simple signals the stream runs below the target.
- **Real time.** Every work buffer is allocated at open. Pushing samples and encoding a
  unit allocate nothing once the input buffers have grown to a stream's working size
  ([R-RT-002](../../docs/RULES.md#R-RT-002)). The crate is built optimised in debug builds
  too, like `jstream-audio`.

**Tests.** The unit tests check the MDCT against its defining sum, the FFT against the
DFT, the windows' power complementarity, the Bessel function, every codebook as a
complete prefix code with the standard's index layout, written bits against counted bits,
escapes, sectioning, the bit writer, and the model's curves, pre-echo control and attack
detector. Through the reference decoder: **every codeword of every codebook** (signs and
escapes too) decodes to the standard's synthesis of the values written. The output also
lags by exactly `DELAY`, and planar and interleaved input give the same stream. Hostile
input (NaN, infinities, huge values, DC, square waves, clicks) in random buffer sizes, at
every rate, layout and window shape, never panics, keeps every unit within the decoder's
buffer, and decodes. Four seconds of full-scale noise at 64, 128 and 192 kbps stay at or
under the target rate and within 10 % of it. The quality tests check waveform SNR floors on
tones and a synthetic music signal at 128 and 192 kbps, the spectral envelope of noise
(within 2.5 dB per third octave), the window sequences (long on steady signals, short
around attacks, and only legal transitions), and pre-echo below an attack. TNS: its
coefficients round-trip through the quantizer, the standard's synthesis filter undoes the
analysis filter, Levinson-Durbin finds a known predictor, a click gets a filter and white
lines don't, and `tns_data()`'s written bits match its counted bits. Clicks inside long
windows engage TNS in at least nine units of ten, decode, and keep the temporal envelope
(per 64-sample block) within 3.6 dB of the input's, at least 1.8 dB closer than without
TNS, at 96 and 128 kbps. The hostile-input test runs with TNS on and off. `tests/no_alloc.rs`
checks that steady-state encoding doesn't allocate.

Not built yet: intensity stereo, short-window grouping, and more than two channels.
