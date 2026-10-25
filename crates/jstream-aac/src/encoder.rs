//! The encoder: input buffering, block switching, the filterbank, temporal noise
//! shaping (`tns`), M/S stereo, rate control, and the `raw_data_block` syntax
//! (ISO/IEC 14496-3 §4.4.2).
//!
//! Rate control is ISO/IEC 13818-7 Annex B's bit reservoir: the mean bits per frame
//! (`bit_rate * 1024 / sample_rate`) flow in every frame, a frame may spend what the
//! reservoir holds on top of them, and the reservoir never holds more than the decoder's
//! input buffer allows (6144 bits per channel, less one frame's mean). How much of it a
//! frame spends follows its perceptual entropy against the running mean, as Annex B
//! suggests. M/S is decided per band by comparing the perceptual entropy of L/R and M/S
//! coding, with the M and S thresholds at half the smaller of the L and R thresholds so
//! that the noise of `L = M + S` stays masked (J. D. Johnston and A. J. Ferreira,
//! "Sum-difference stereo transform coding", ICASSP 1992). Intensity stereo (`intensity`)
//! is decided before M/S, on bands no TNS filter touches, and M/S is off on its bands.

use crate::bits::BitWriter;
use crate::huffman::{self, ZERO_HCB};
use crate::intensity;
use crate::mdct::{Mdct, WindowShape, Windows};
use crate::psy::{self, AttackDetector, Layout};
use crate::quant::{Channel, MAX_BANDS, Quantizer, Shape};
use crate::tables::{SWB_LONG, SWB_SHORT};
use crate::tns::{self, Analyser};
use crate::{Config, Error, FRAME_LEN};

/// Samples the encoder holds before it encodes a frame: the frame's 2048-sample window
/// and the 576 after it, where the attack detector looks for the next frame's attacks.
const NEED: usize = 2 * FRAME_LEN + 576;
/// The decoder input buffer per channel, in bits (ISO/IEC 14496-3 §4.5.3.1).
const BUFFER_BITS: u32 = 6144;
/// Syntax element IDs (14496-3 Table 4.85).
const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_END: u32 = 7;

/// `window_sequence` (14496-3 Table 4.88).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sequence {
    OnlyLong = 0,
    LongStart = 1,
    EightShort = 2,
    LongStop = 3,
}

impl Sequence {
    /// Whether the window's right half is short, so the next one must start short.
    fn ends_short(self) -> bool {
        matches!(self, Self::LongStart | Self::EightShort)
    }

    /// The next window sequence, given whether this frame and the next hold attacks.
    fn next(prev: Self, attack: bool, attack_next: bool) -> Self {
        if prev.ends_short() {
            if attack || attack_next {
                Self::EightShort
            } else {
                Self::LongStop
            }
        } else if attack_next {
            Self::LongStart
        } else {
            Self::OnlyLong
        }
    }
}

/// An AAC-LC encoder: interleaved or planar `f32` samples in, raw access units out.
///
/// The decoder's output lags the input by [`DELAY`](crate::DELAY) samples, the usual
/// AAC priming: the first access unit decodes to silence.
pub struct Encoder {
    config: Config,
    channels: usize,
    /// The samples from the next frame's window start on, per channel, in 16-bit units.
    input: Vec<Vec<f32>>,
    /// Real samples pushed per channel (priming and padding excluded).
    pushed: u64,
    /// Access units produced so far.
    produced: u64,
    finished: bool,
    windows: Windows,
    long: Mdct,
    short: Mdct,
    layout_long: Layout,
    layout_short: Layout,
    quantizer: Quantizer,
    tns: Analyser,
    detector: AttackDetector,
    /// Whether the next frame's short-window region holds an attack.
    attack_next: Option<bool>,
    prev_sequence: Sequence,
    prev_shape: WindowShape,
    /// The previous long block's thresholds per channel, for pre-echo control.
    prev_thr: Vec<[f32; 49]>,
    state: Vec<Channel>,
    ms: [bool; MAX_BANDS],
    /// Where intensity stereo starts, in Hz, and its slack over the masking threshold
    /// (`intensity::start`); `None` when it is off.
    intensity: Option<(f64, f32)>,
    /// Mean bits per frame, and the reservoir's fill and size.
    mean_bits: u32,
    reservoir: u32,
    reservoir_max: u32,
    /// A running mean of the frames' perceptual entropy.
    pe_mean: f32,
    windowed: Vec<f32>,
}

/// The bandwidth coded at `bits_per_channel` bits per second per channel: about 16 kHz
/// at 64 kbps, rising to 20 kHz.
fn bandwidth(bits_per_channel: u32, sample_rate: u32) -> f64 {
    (5000.0 + 0.17 * f64::from(bits_per_channel))
        .clamp(8000.0, 20_000.0)
        .min(f64::from(sample_rate) / 2.0)
}

impl Encoder {
    /// An encoder for `config`.
    ///
    /// # Errors
    /// [`Error::SampleRate`], [`Error::Channels`] or [`Error::BitRate`] when the
    /// configuration is outside what the encoder supports (see [`Config`]).
    pub fn new(config: Config) -> Result<Self, Error> {
        config.check()?;
        let channels = usize::from(config.channels);
        let per_channel = config.bit_rate / u32::from(config.channels);
        let bw = bandwidth(per_channel, config.sample_rate);
        let mean_bits =
            (u64::from(config.bit_rate) * FRAME_LEN as u64 / u64::from(config.sample_rate)) as u32;
        let max_frame = BUFFER_BITS * u32::from(config.channels);
        Ok(Self {
            config,
            channels,
            input: vec![vec![0.0; FRAME_LEN]; channels],
            pushed: 0,
            produced: 0,
            finished: false,
            windows: Windows::new(),
            long: Mdct::new(FRAME_LEN),
            short: Mdct::new(128),
            layout_long: Layout::new(&SWB_LONG, 1024, config.sample_rate, bw),
            layout_short: Layout::new(&SWB_SHORT, 128, config.sample_rate, bw),
            quantizer: Quantizer::new(),
            tns: Analyser::new(config.sample_rate, &SWB_LONG, &SWB_SHORT),
            detector: AttackDetector::default(),
            attack_next: None,
            prev_sequence: Sequence::OnlyLong,
            prev_shape: WindowShape::Sine,
            prev_thr: vec![[0.0; 49]; channels],
            state: (0..channels).map(|_| Channel::new()).collect(),
            ms: [false; MAX_BANDS],
            intensity: if config.intensity && channels == 2 {
                intensity::start(per_channel)
            } else {
                None
            },
            mean_bits,
            reservoir: 0,
            reservoir_max: max_frame.saturating_sub(mean_bits),
            pe_mean: 0.0,
            windowed: vec![0.0; 2 * FRAME_LEN],
        })
    }

    /// Whether the last access unit sent TNS data on any channel.
    #[cfg(test)]
    pub(crate) fn used_tns(&self) -> bool {
        self.state
            .iter()
            .any(|ch| ch.tns.filters.iter().any(|f| f.order > 0))
    }

    /// How many bands of the last access unit were intensity coded.
    #[cfg(test)]
    pub(crate) fn intensity_bands(&self) -> usize {
        self.state
            .iter()
            .map(|ch| ch.is_book.iter().filter(|&&b| b != 0).count())
            .sum()
    }

    /// The configuration the encoder was opened with.
    pub fn config(&self) -> Config {
        self.config
    }

    /// The `AudioSpecificConfig` for the stream (ISO/IEC 14496-3 §1.6.2.1): AAC-LC, the
    /// sample rate's index and the channel configuration, with a 1024-sample frame. FLV
    /// and MP4 carry it once ahead of the access units.
    pub fn audio_specific_config(&self) -> [u8; 2] {
        let rate_index: u16 = if self.config.sample_rate == 48_000 {
            3
        } else {
            4
        };
        let v = (2u16 << 11) | (rate_index << 7) | (u16::from(self.config.channels) << 3);
        v.to_be_bytes()
    }

    fn check_open(&self) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Finished);
        }
        Ok(())
    }

    /// Converts a sample to 16-bit units, clamping to full scale; NaN becomes silence.
    fn scale(s: f32) -> f32 {
        if s.is_nan() {
            0.0
        } else {
            s.clamp(-1.0, 1.0) * 32768.0
        }
    }

    /// Queues interleaved samples (`L R L R …` for stereo).
    ///
    /// # Errors
    /// [`Error::Finished`] after [`finish`](Self::finish), and [`Error::PartialFrame`]
    /// when `samples` isn't a whole number of frames.
    pub fn push_interleaved(&mut self, samples: &[f32]) -> Result<(), Error> {
        self.check_open()?;
        if !samples.len().is_multiple_of(self.channels) {
            return Err(Error::PartialFrame {
                samples: samples.len(),
                channels: self.config.channels,
            });
        }
        for (c, buf) in self.input.iter_mut().enumerate() {
            buf.extend(
                samples
                    .iter()
                    .skip(c)
                    .step_by(self.channels)
                    .map(|&s| Self::scale(s)),
            );
        }
        self.pushed += (samples.len() / self.channels) as u64;
        Ok(())
    }

    /// Queues planar samples: every sample of channel 0, then of channel 1.
    ///
    /// # Errors
    /// As for [`push_interleaved`](Self::push_interleaved).
    pub fn push_planar(&mut self, samples: &[f32]) -> Result<(), Error> {
        self.check_open()?;
        if !samples.len().is_multiple_of(self.channels) {
            return Err(Error::PartialFrame {
                samples: samples.len(),
                channels: self.config.channels,
            });
        }
        let frames = samples.len() / self.channels;
        for (c, buf) in self.input.iter_mut().enumerate() {
            buf.extend(
                samples[c * frames..(c + 1) * frames]
                    .iter()
                    .map(|&s| Self::scale(s)),
            );
        }
        self.pushed += frames as u64;
        Ok(())
    }

    /// Ends the input: the remaining samples are encoded, padded with silence, and one
    /// more access unit follows so that the decoder outputs the last of them. Afterwards
    /// [`encode_frame`](Self::encode_frame) drains the encoder.
    pub fn finish(&mut self) {
        self.finished = true;
    }

    /// The access units the stream will have in all, once finished: enough to cover the
    /// samples pushed, plus the priming delay.
    fn total_frames(&self) -> u64 {
        if self.pushed == 0 {
            0
        } else {
            self.pushed.div_ceil(FRAME_LEN as u64) + 1
        }
    }

    /// Access units ready to encode now.
    pub fn ready(&self) -> usize {
        if self.finished {
            (self.total_frames() - self.produced) as usize
        } else {
            let have = self.input[0].len();
            if have < NEED {
                0
            } else {
                (have - NEED) / FRAME_LEN + 1
            }
        }
    }

    /// Encodes the next access unit, if one is ready, and appends it to `out`. Returns
    /// whether it did.
    pub fn encode_frame(&mut self, out: &mut Vec<u8>) -> bool {
        if self.ready() == 0 {
            return false;
        }
        for buf in &mut self.input {
            if buf.len() < NEED {
                buf.resize(NEED, 0.0);
            }
        }
        self.encode(out);
        for buf in &mut self.input {
            buf.drain(..FRAME_LEN);
        }
        self.produced += 1;
        true
    }

    /// Encodes the frame whose window starts at `input[c][0]`.
    fn encode(&mut self, out: &mut Vec<u8>) {
        let attack = match self.attack_next {
            Some(a) => a,
            None => {
                let views: [&[f32]; 2] = [
                    self.input[0].as_slice(),
                    self.input[self.channels - 1].as_slice(),
                ];
                self.detector.attack(&views[..self.channels], 576)
            }
        };
        let attack_next = {
            let views: [&[f32]; 2] = [
                self.input[0].as_slice(),
                self.input[self.channels - 1].as_slice(),
            ];
            self.detector.attack(&views[..self.channels], 1600)
        };
        self.attack_next = Some(attack_next);
        let sequence = Sequence::next(self.prev_sequence, attack, attack_next);
        let shape = self.config.window_shape;
        let short = sequence == Sequence::EightShort;
        let (offsets, bands, windows, lines): (&'static [u16], usize, usize, usize) = if short {
            (&SWB_SHORT, self.layout_short.bands, 8, 128)
        } else {
            (&SWB_LONG, self.layout_long.bands, 1, 1024)
        };
        let frame = Shape {
            offsets,
            bands,
            windows,
            lines,
        };

        // Filterbank, model and TNS. Each channel is filtered before M/S, since a decoder
        // undoes M/S first and TNS after.
        let mut pe = [0.0f32; 2];
        for (c, pe) in pe.iter_mut().enumerate().take(self.channels) {
            self.analyse(c, sequence, shape);
            *pe = self.model(c, &frame, short);
            if let Some(shaped) = self.shape_noise(c, &frame, short) {
                *pe = shaped;
            }
        }
        if self.channels == 2 {
            self.decide_intensity(&frame);
            pe = self.decide_ms(&frame);
        } else {
            self.ms = [false; MAX_BANDS];
        }

        // Rate control: the frame's budget, then each channel's share.
        let budget = self.budget(pe[0] + pe[1]);
        let ics_info = if short { 15 } else { 11 };
        let ms_bits = self.ms_bits(&frame);
        let header = if self.channels == 2 {
            3 + 4 + 1 + ics_info + 2 + ms_bits
        } else {
            3 + 4
        };
        // global_gain, the pulse, TNS and gain control flags, and ics_info for an SCE.
        let fixed = 8 + 3 + if self.channels == 1 { ics_info } else { 0 };
        let trailer = 3 + 7;
        let mut left = budget.saturating_sub(header + trailer);
        for c in 0..self.channels {
            let fixed = fixed + self.state[c].tns.bits(&frame);
            let share = if c + 1 == self.channels {
                1.0
            } else {
                let total = pe[0] + pe[1];
                if total > 0.0 {
                    (pe[0] / total).clamp(0.25, 0.75)
                } else {
                    0.5
                }
            };
            let ch_budget = ((left as f32 * share) as u32).saturating_sub(fixed);
            let used = self.quantizer.run(&mut self.state[c], &frame, ch_budget) + fixed;
            left = left.saturating_sub(used);
        }

        // Bitstream.
        let start = out.len();
        let mut w = BitWriter::new(out);
        if self.channels == 2 {
            w.put(ID_CPE, 3);
            w.put(0, 4);
            w.bit(true); // common_window
            write_ics_info(&mut w, sequence, shape, &frame);
            let n = frame.count();
            let any = self.ms[..n].iter().any(|&m| m);
            let all = self.ms[..n].iter().all(|&m| m);
            if !any {
                w.put(0, 2);
            } else if all {
                w.put(2, 2);
            } else {
                w.put(1, 2);
                for &m in &self.ms[..n] {
                    w.bit(m);
                }
            }
            for c in 0..2 {
                write_ics(&mut w, &self.state[c], &frame, None);
            }
        } else {
            w.put(ID_SCE, 3);
            w.put(0, 4);
            write_ics(&mut w, &self.state[0], &frame, Some((sequence, shape)));
        }
        w.put(ID_END, 3);
        let bits = w.finish() as u32;
        debug_assert_eq!(bits as usize, (out.len() - start) * 8);

        self.reservoir = (self.reservoir + self.mean_bits)
            .saturating_sub(bits)
            .min(self.reservoir_max);
        self.prev_sequence = sequence;
        self.prev_shape = shape;
    }

    /// Windows and transforms channel `c`'s current frame into its spectrum.
    fn analyse(&mut self, c: usize, sequence: Sequence, shape: WindowShape) {
        let x = &self.input[c];
        let spec = &mut self.state[c].spec;
        let wl_prev = self.windows.long(self.prev_shape);
        let wl = self.windows.long(shape);
        let ws_prev = self.windows.short(self.prev_shape);
        let ws = self.windows.short(shape);
        let out = &mut self.windowed;
        match sequence {
            Sequence::EightShort => {
                let mut seg = [0.0f32; 256];
                for win in 0..8 {
                    let base = 448 + 128 * win;
                    let rise = if win == 0 { ws_prev } else { ws };
                    for i in 0..128 {
                        seg[i] = x[base + i] * rise[i];
                        seg[128 + i] = x[base + 128 + i] * ws[127 - i];
                    }
                    self.short
                        .forward(&seg, &mut spec[win * 128..(win + 1) * 128]);
                }
                return;
            }
            Sequence::OnlyLong => {
                for i in 0..1024 {
                    out[i] = x[i] * wl_prev[i];
                    out[1024 + i] = x[1024 + i] * wl[1023 - i];
                }
            }
            Sequence::LongStart => {
                for i in 0..1024 {
                    out[i] = x[i] * wl_prev[i];
                }
                out[1024..1472].copy_from_slice(&x[1024..1472]);
                for i in 0..128 {
                    out[1472 + i] = x[1472 + i] * ws[127 - i];
                }
                out[1600..].fill(0.0);
            }
            Sequence::LongStop => {
                out[..448].fill(0.0);
                for i in 0..128 {
                    out[448 + i] = x[448 + i] * ws_prev[i];
                }
                out[576..1024].copy_from_slice(&x[576..1024]);
                for i in 0..1024 {
                    out[1024 + i] = x[1024 + i] * wl[1023 - i];
                }
            }
        }
        self.long.forward(out, &mut spec[..1024]);
    }

    /// Runs the psychoacoustic model on channel `c`'s spectrum: band energies and
    /// thresholds, and the perceptual entropy it returns.
    fn model(&mut self, c: usize, frame: &Shape<'_>, short: bool) -> f32 {
        let layout = if short {
            &self.layout_short
        } else {
            &self.layout_long
        };
        let ch = &mut self.state[c];
        let (bands, lines) = (frame.bands, frame.lines);
        let mut pe = 0.0;
        for w in 0..frame.windows {
            let spec = &ch.spec[w * lines..(w + 1) * lines];
            let range = w * bands..(w + 1) * bands;
            psy::band_energies(layout, spec, &mut ch.energy[range.clone()]);
            let prev = (!short).then_some(&mut self.prev_thr[c][..bands]);
            psy::thresholds(
                layout,
                spec,
                &ch.energy[range.clone()],
                &mut ch.thr[range.clone()],
                prev,
            );
            pe += psy::entropy(layout, &ch.energy[range.clone()], &ch.thr[range]);
        }
        if short {
            // A long block after short ones isn't limited by stale thresholds.
            self.prev_thr[c] = [0.0; 49];
        }
        pe
    }

    /// Filters channel `c`'s spectrum with temporal noise shaping where it pays (`tns`),
    /// and returns the perceptual entropy of the filtered spectrum, whose band energies
    /// replace the model's. The thresholds are the unfiltered spectrum's, divided in the
    /// filtered bands by the decoder filter's noise gain. `None` when no window is
    /// filtered.
    fn shape_noise(&mut self, c: usize, frame: &Shape<'_>, short: bool) -> Option<f32> {
        let ch = &mut self.state[c];
        if !self.config.tns {
            ch.tns = Default::default();
            return None;
        }
        // Only windows whose envelope is uneven in time: on a stationary one TNS would
        // spread the noise between tonal peaks.
        let x = &self.input[c];
        let mut gate = [false; 8];
        if short {
            for (w, g) in gate.iter_mut().enumerate() {
                *g = tns::uneven(&x[448 + 128 * w..704 + 128 * w]);
            }
        } else {
            gate[0] = tns::uneven(&x[..2048]);
        }
        let mut gain = [1.0; 8];
        self.tns
            .apply(&mut ch.spec, frame, &gate, &mut ch.tns, &mut gain);
        if !ch.tns.present(frame) {
            return None;
        }
        // The decoder's filter amplifies the residual's quantization noise, so the
        // filtered bands are quantized against thresholds lowered by its gain.
        let (bands, lines) = (frame.bands, frame.lines);
        for (w, &g) in gain[..frame.windows].iter().enumerate() {
            for b in self.tns.bands(frame) {
                ch.thr[w * bands + b] /= g;
            }
        }
        let layout = if short {
            &self.layout_short
        } else {
            &self.layout_long
        };
        let mut pe = 0.0;
        for w in 0..frame.windows {
            let spec = &ch.spec[w * lines..(w + 1) * lines];
            let range = w * bands..(w + 1) * bands;
            psy::band_energies(layout, spec, &mut ch.energy[range.clone()]);
            pe += psy::entropy(layout, &ch.energy[range.clone()], &ch.thr[range]);
        }
        Some(pe)
    }

    /// Chooses the bands of a CPE that are coded as intensity stereo (`intensity`), and
    /// sets them up: the left band holds the coded sum, with the threshold that keeps its
    /// noise masked in both channels once the decoder scales it into the right one; the
    /// right band is empty, with no energy to code, and names its codebook and position.
    /// None in a frame where either channel has a TNS filter: a decoder runs each
    /// channel's filter after rebuilding the right band from the left, so the right
    /// channel's filter (or its absence) would apply to the left channel's residual.
    fn decide_intensity(&mut self, frame: &Shape<'_>) {
        let (a, b) = self.state.split_at_mut(1);
        let (l, r) = (&mut a[0], &mut b[0]);
        r.is_book = [0; MAX_BANDS];
        let Some((start_hz, slack)) = self.intensity else {
            return;
        };
        if l.tns.present(frame) || r.tns.present(frame) {
            return;
        }
        let hz_per_line = f64::from(self.config.sample_rate) / (2.0 * frame.lines as f64);
        for i in 0..frame.count() {
            let band = i % frame.bands;
            if f64::from(frame.offsets[band]) * hz_per_line < start_hz {
                continue;
            }
            let range = frame.range(i);
            let thr = [l.thr[i], r.thr[i]];
            let Some(coded) = intensity::code(
                &mut l.spec[range.clone()],
                &r.spec[range.clone()],
                thr,
                slack,
            ) else {
                continue;
            };
            let el = l.energy[i];
            let er = r.energy[i];
            l.thr[i] = l.thr[i].min(r.thr[i] * el / er);
            l.energy[i] = l.spec[range.clone()].iter().map(|x| x * x).sum();
            r.spec[range].fill(0.0);
            r.energy[i] = 0.0;
            r.is_book[i] = coded.book();
            r.is_position[i] = coded.position;
        }
    }

    /// Chooses L/R or M/S per band, converts the M/S bands' spectra, energies and
    /// thresholds, and returns each channel's perceptual entropy afterwards.
    fn decide_ms(&mut self, frame: &Shape<'_>) -> [f32; 2] {
        let (a, b) = self.state.split_at_mut(1);
        let (l, r) = (&mut a[0], &mut b[0]);
        let mut pe = [0.0f32; 2];
        let bits = |e: f32, t: f32, width: f32| 0.5 * width * (1.0 + e / t.max(1e-9)).log2();
        for i in 0..frame.count() {
            let range = frame.range(i);
            let width = range.len() as f32;
            if r.is_book[i] != 0 {
                // Intensity coded: the right channel has nothing to code here.
                self.ms[i] = false;
                pe[0] += bits(l.energy[i], l.thr[i], width);
                continue;
            }
            let (mut em, mut es) = (0.0f32, 0.0f32);
            for k in range.clone() {
                let m = 0.5 * (l.spec[k] + r.spec[k]);
                let s = 0.5 * (l.spec[k] - r.spec[k]);
                em += m * m;
                es += s * s;
            }
            let t = 0.5 * l.thr[i].min(r.thr[i]);
            let pe_lr = bits(l.energy[i], l.thr[i], width) + bits(r.energy[i], r.thr[i], width);
            let pe_ms = bits(em, t, width) + bits(es, t, width);
            let ms = pe_ms < pe_lr;
            self.ms[i] = ms;
            if ms {
                for k in range {
                    let (x, y) = (l.spec[k], r.spec[k]);
                    l.spec[k] = 0.5 * (x + y);
                    r.spec[k] = 0.5 * (x - y);
                }
                l.energy[i] = em;
                r.energy[i] = es;
                l.thr[i] = t;
                r.thr[i] = t;
                pe[0] += bits(em, t, width);
                pe[1] += bits(es, t, width);
            } else {
                pe[0] += bits(l.energy[i], l.thr[i], width);
                pe[1] += bits(r.energy[i], r.thr[i], width);
            }
        }
        pe
    }

    /// The M/S mask's bits: one per band when some bands, but not all, use M/S.
    fn ms_bits(&self, frame: &Shape<'_>) -> u32 {
        if self.channels < 2 {
            return 0;
        }
        let n = frame.count();
        let any = self.ms[..n].iter().any(|&m| m);
        let all = self.ms[..n].iter().all(|&m| m);
        if any && !all { n as u32 } else { 0 }
    }

    /// The frame's bit budget for perceptual entropy `pe`: the mean, scaled by how the
    /// frame's demand compares with the running mean, within what the reservoir allows
    /// (it can't lend more than it holds, nor keep more than it can hold).
    fn budget(&mut self, pe: f32) -> u32 {
        if self.pe_mean <= 0.0 {
            self.pe_mean = pe.max(1.0);
        }
        let ratio = (pe / self.pe_mean).clamp(0.5, 2.0);
        self.pe_mean = 0.9 * self.pe_mean + 0.1 * pe.max(1.0);
        let mean = self.mean_bits as f32;
        let mut want = mean * ratio;
        // A reservoir past half full is spent down, so the stream keeps its rate.
        let half = self.reservoir_max as f32 / 2.0;
        if (self.reservoir as f32) > half {
            want += self.reservoir as f32 - half;
        }
        let lower = self
            .mean_bits
            .saturating_sub(self.reservoir_max - self.reservoir);
        let upper = (self.mean_bits + self.reservoir).min(BUFFER_BITS * self.channels as u32);
        (want as u32).clamp(lower, upper)
    }
}

/// Writes `ics_info()` (14496-3 Table 4.6) for `frame`.
pub(crate) fn write_ics_info(
    w: &mut BitWriter<'_>,
    sequence: Sequence,
    shape: WindowShape,
    frame: &Shape<'_>,
) {
    w.bit(false); // ics_reserved_bit
    w.put(sequence as u32, 2);
    w.bit(shape.bit());
    if sequence == Sequence::EightShort {
        w.put(frame.bands as u32, 4);
        // Every short window its own group.
        w.put(0, 7);
    } else {
        w.put(frame.bands as u32, 6);
        w.bit(false); // predictor_data_present
    }
}

/// Writes `individual_channel_stream()` (14496-3 Table 4.50). `info` is the window
/// sequence and shape to write `ics_info` with, for a channel without a common window.
pub(crate) fn write_ics(
    w: &mut BitWriter<'_>,
    ch: &Channel,
    frame: &Shape<'_>,
    info: Option<(Sequence, WindowShape)>,
) {
    w.put(ch.global_gain as u32, 8);
    if let Some((sequence, shape)) = info {
        write_ics_info(w, sequence, shape, frame);
    }
    let len_bits = frame.len_bits();
    for g in 0..frame.windows {
        huffman::write_sections(&ch.sections[g][..ch.section_count[g]], len_bits, w);
    }
    let mut last = ch.global_gain;
    let mut last_position = 0;
    for g in 0..frame.windows {
        for s in &ch.sections[g][..ch.section_count[g]] {
            if s.book == ZERO_HCB {
                continue;
            }
            let is = intensity::is_intensity(s.book);
            for b in s.start..s.end {
                let i = g * frame.bands + usize::from(b);
                if is {
                    let p = ch.is_position[i];
                    huffman::write_sf(p - last_position, w);
                    last_position = p;
                } else {
                    let sf = ch.sf[i];
                    huffman::write_sf(sf - last, w);
                    last = sf;
                }
            }
        }
    }
    w.bit(false); // pulse_data_present
    let tns = ch.tns.present(frame);
    w.bit(tns);
    if tns {
        ch.tns.write(w, frame);
    }
    w.bit(false); // gain_control_data_present
    for g in 0..frame.windows {
        for s in &ch.sections[g][..ch.section_count[g]] {
            if s.book == ZERO_HCB || intensity::is_intensity(s.book) {
                continue;
            }
            for b in s.start..s.end {
                let range = frame.range(g * frame.bands + usize::from(b));
                huffman::write_band(s.book, &ch.q[range], w);
            }
        }
    }
}
