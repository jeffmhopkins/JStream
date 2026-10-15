//! Filter chains on inputs and buses (docs/adr/0028-audio-model-v2.md).
//!
//! Every input and every bus has an ordered chain of up to [`MAX_FILTERS`] filters built
//! from the kernels in [`crate::filter`]. The control plane builds each filter (which
//! allocates its state) and sends it to the mixer through the command queue; the mixer
//! inserts it into a list reserved when the chain was built, so installing never
//! allocates. A removed filter, and a removed input's or bus's whole chain, goes to the
//! collector to be freed (R-RT-003). Parameter edits travel as plain values and are
//! applied with the kernels' own setters, which don't allocate.
//!
//! **Latency compensation.** Each chain reports the sum of its filters' latencies. The
//! mixer delays every input to the longest input chain's latency, and every bus to the
//! longest bus chain's, with a compensation delay after the chain, so all inputs line up
//! in every bus sum and all buses line up with each other. A [`Delay`] filter is a
//! deliberate shift with no latency, so it is not compensated.

use crate::AudioError;
use crate::filter::{
    AudioFilter, Compressor, CompressorParams, Delay, Gain, GateParams, Limiter, LimiterParams,
    NoiseGate,
};

/// Filters one chain can hold. Its list is reserved at this size when it is built.
pub(crate) const MAX_FILTERS: usize = 8;

/// The longest latency a chain may add, ms. Each chain's compensation delay is
/// allocated for this when the chain is built.
pub(crate) const MAX_CHAIN_LATENCY_MS: f32 = 100.0;

/// A filter to add to an input's or a bus's chain. The mixer builds it at its engine
/// rate.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum FilterSpec {
    /// [`Gain`], −120 to +24 dB or `-inf`.
    Gain {
        /// The gain, dB.
        gain_db: f32,
    },
    /// [`NoiseGate`].
    NoiseGate(GateParams),
    /// [`Compressor`], without a sidechain key.
    Compressor(CompressorParams),
    /// [`Limiter`] with a look-ahead fixed when it is built: its latency.
    Limiter {
        /// The look-ahead, 0 to 20 ms.
        lookahead_ms: f32,
        /// The limiter's settings.
        params: LimiterParams,
    },
    /// [`Delay`]: a deliberate shift, not compensated.
    Delay {
        /// The longest delay the filter can be set to later, ms (at most 5000).
        max_ms: f32,
        /// The delay it starts with, ms.
        delay_ms: f32,
    },
}

/// New settings for a filter already in a chain. The variant must match the filter's
/// kind.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum FilterParams {
    /// A [`FilterSpec::Gain`]'s gain, dB.
    Gain {
        /// The gain, dB.
        gain_db: f32,
    },
    /// A [`FilterSpec::NoiseGate`]'s settings.
    NoiseGate(GateParams),
    /// A [`FilterSpec::Compressor`]'s settings.
    Compressor(CompressorParams),
    /// A [`FilterSpec::Limiter`]'s settings (its look-ahead stays fixed).
    Limiter(LimiterParams),
    /// A [`FilterSpec::Delay`]'s delay, ms, up to the maximum it was built with.
    Delay {
        /// The delay, ms.
        delay_ms: f32,
    },
}

/// Which kind of kernel a filter is, so the control plane can check an edit's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KernelKind {
    Gain,
    NoiseGate,
    Compressor,
    Limiter,
    Delay,
}

impl FilterParams {
    pub(crate) fn kind(&self) -> KernelKind {
        match self {
            Self::Gain { .. } => KernelKind::Gain,
            Self::NoiseGate(_) => KernelKind::NoiseGate,
            Self::Compressor(_) => KernelKind::Compressor,
            Self::Limiter(_) => KernelKind::Limiter,
            Self::Delay { .. } => KernelKind::Delay,
        }
    }
}

/// One filter in a chain.
#[derive(Debug)]
pub(crate) enum Kernel {
    Gain(Gain),
    NoiseGate(NoiseGate),
    Compressor(Compressor),
    Limiter(Limiter),
    Delay(Delay),
}

impl Kernel {
    /// Builds the filter `spec` describes at `rate`. Allocates: control thread only.
    pub(crate) fn build(rate: u32, spec: FilterSpec) -> Result<Self, AudioError> {
        Ok(match spec {
            FilterSpec::Gain { gain_db } => Self::Gain(Gain::new(rate, gain_db)?),
            FilterSpec::NoiseGate(p) => Self::NoiseGate(NoiseGate::new(rate, p)?),
            FilterSpec::Compressor(p) => Self::Compressor(Compressor::new(rate, p)?),
            FilterSpec::Limiter {
                lookahead_ms,
                params,
            } => Self::Limiter(Limiter::new(rate, lookahead_ms, params)?),
            FilterSpec::Delay { max_ms, delay_ms } => {
                let mut delay = Delay::new(rate, max_ms)?;
                delay.set_delay_ms(delay_ms)?;
                Self::Delay(delay)
            }
        })
    }

    pub(crate) fn kind(&self) -> KernelKind {
        match self {
            Self::Gain(_) => KernelKind::Gain,
            Self::NoiseGate(_) => KernelKind::NoiseGate,
            Self::Compressor(_) => KernelKind::Compressor,
            Self::Limiter(_) => KernelKind::Limiter,
            Self::Delay(_) => KernelKind::Delay,
        }
    }

    fn filter(&mut self) -> &mut dyn AudioFilter {
        match self {
            Self::Gain(f) => f,
            Self::NoiseGate(f) => f,
            Self::Compressor(f) => f,
            Self::Limiter(f) => f,
            Self::Delay(f) => f,
        }
    }

    pub(crate) fn latency_frames(&self) -> usize {
        match self {
            Self::Gain(f) => f.latency_frames(),
            Self::NoiseGate(f) => f.latency_frames(),
            Self::Compressor(f) => f.latency_frames(),
            Self::Limiter(f) => f.latency_frames(),
            Self::Delay(f) => f.latency_frames(),
        }
    }

    /// Applies `params`, already checked by [`Self::check`] on the control plane. Does not
    /// allocate. Parameters of another kind are ignored.
    fn set(&mut self, params: &FilterParams) {
        // The control plane checked these, so an error can't happen; if one did, the
        // filter keeps its previous settings.
        let _ = match (self, params) {
            (Self::Gain(g), FilterParams::Gain { gain_db }) => g.set_gain_db(*gain_db),
            (Self::NoiseGate(g), FilterParams::NoiseGate(p)) => g.set_params(*p),
            (Self::Compressor(c), FilterParams::Compressor(p)) => c.set_params(*p),
            (Self::Limiter(l), FilterParams::Limiter(p)) => l.set_params(*p),
            (Self::Delay(d), FilterParams::Delay { delay_ms }) => d.set_delay_ms(*delay_ms),
            _ => Ok(()),
        };
    }
}

/// What the control plane keeps about a filter it sent to the mixer, to check edits
/// without the kernel itself.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterBook {
    pub(crate) id: u64,
    pub(crate) kind: KernelKind,
    pub(crate) latency: usize,
    /// For a delay, the longest delay it has room for, frames.
    pub(crate) max_delay_frames: usize,
}

impl FilterBook {
    pub(crate) fn new(id: u64, kernel: &Kernel) -> Self {
        Self {
            id,
            kind: kernel.kind(),
            latency: kernel.latency_frames(),
            max_delay_frames: match kernel {
                Kernel::Delay(d) => d.max_frames(),
                _ => 0,
            },
        }
    }

    /// Checks `params` for this filter at `rate`: the right kind, in range.
    pub(crate) fn check(&self, rate: u32, params: &FilterParams) -> Result<(), AudioError> {
        if params.kind() != self.kind {
            return Err(AudioError::InvalidConfig {
                what: "filter parameters must match the filter's kind",
            });
        }
        match params {
            FilterParams::Gain { gain_db } => Gain::new(rate, *gain_db).map(drop),
            FilterParams::NoiseGate(p) => p.validate(),
            FilterParams::Compressor(p) => p.validate(),
            FilterParams::Limiter(p) => p.validate(),
            FilterParams::Delay { delay_ms } => {
                let frames = f64::from(*delay_ms) * f64::from(rate) / 1000.0;
                if delay_ms.is_finite()
                    && *delay_ms >= 0.0
                    && frames.round() <= self.max_delay_frames as f64
                {
                    Ok(())
                } else {
                    Err(AudioError::InvalidConfig {
                        what: "delay must be 0 up to the maximum the filter was built for",
                    })
                }
            }
        }
    }
}

/// One input's or bus's filter chain, with its compensation delay. Built on the control
/// thread; owned by the mixer thread once installed.
pub(crate) struct Chain {
    /// The filters in order, each with its id. Reserved for [`MAX_FILTERS`].
    filters: Vec<(u64, Box<Kernel>)>,
    /// The sum of the filters' latencies, frames.
    latency: usize,
    /// Delays this chain's output so it lines up with the longest chain beside it.
    comp: Delay,
}

impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chain")
            .field("filters", &self.filters.len())
            .field("latency", &self.latency)
            .field("comp", &self.comp.delay_frames())
            .finish()
    }
}

impl Chain {
    /// An empty chain for a mixer at `rate`. Allocates its filter list and its
    /// compensation delay: control thread only.
    pub(crate) fn new(rate: u32) -> Result<Self, AudioError> {
        Ok(Self {
            filters: Vec::with_capacity(MAX_FILTERS),
            latency: 0,
            comp: Delay::new(rate, MAX_CHAIN_LATENCY_MS)?,
        })
    }

    /// The sum of the chain's filter latencies, frames.
    pub(crate) fn latency(&self) -> usize {
        self.latency
    }

    /// Runs the filters in order over `block`, then delays it by `comp` frames (capped
    /// at the compensation delay's room).
    pub(crate) fn process(&mut self, block: &mut [[f32; 2]], comp: usize) {
        let flat = block.as_flattened_mut();
        for (_, kernel) in &mut self.filters {
            kernel.filter().process(flat);
        }
        let comp = comp.min(self.comp.max_frames());
        let was = self.comp.delay_frames();
        if comp != was {
            if was == 0 {
                // The delay line didn't run while it was 0: start it from silence.
                self.comp.reset();
            }
            let _ = self.comp.set_delay_frames(comp);
        }
        if comp > 0 {
            self.comp.process(flat);
        }
    }

    /// Inserts `kernel` at `index` (clamped to the end). Gives it back when the chain is
    /// full, so the caller can retire it. Never allocates.
    pub(crate) fn insert(
        &mut self,
        index: usize,
        id: u64,
        kernel: Box<Kernel>,
    ) -> Result<(), Box<Kernel>> {
        if self.filters.len() >= MAX_FILTERS {
            return Err(kernel);
        }
        self.latency += kernel.latency_frames();
        let index = index.min(self.filters.len());
        self.filters.insert(index, (id, kernel));
        Ok(())
    }

    /// Takes filter `id` out of the chain.
    pub(crate) fn remove(&mut self, id: u64) -> Option<Box<Kernel>> {
        let index = self.filters.iter().position(|(f, _)| *f == id)?;
        let (_, kernel) = self.filters.remove(index);
        self.latency -= kernel.latency_frames();
        Some(kernel)
    }

    /// Applies `params` to filter `id`, if it is in the chain.
    pub(crate) fn edit(&mut self, id: u64, params: &FilterParams) {
        if let Some((_, kernel)) = self.filters.iter_mut().find(|(f, _)| *f == id) {
            kernel.set(params);
        }
    }
}
