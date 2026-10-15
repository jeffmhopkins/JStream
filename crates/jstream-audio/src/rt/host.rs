//! The mixer's side of a CLAP filter hosted out of process
//! (docs/adr/0035-clap-plugins-out-of-process.md).
//!
//! A [`HostFilter`] sits in a chain like any kernel. Each block it writes its dry input
//! into the shared input ring as block `n` and reads the host's processed block
//! `n − depth` from the output ring. It never waits on the host: when that block isn't
//! there (the host is late, hung or dead) it passes its own dry signal, delayed by the
//! same latency so timing doesn't jump, crossfading at each switch, and counts the miss
//! in the status block for the control thread to report. Its latency is the pipeline
//! depth plus the plugin's own, fixed when it is built.
//!
//! Everything here is plain loads and stores on the shared block: no syscall, lock or
//! allocation after [`HostFilter::new`] (R-RT-001).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use jstream_plugin_proto::Shared;

use super::chain::MAX_CHAIN_LATENCY_MS;
use crate::AudioError;
use crate::filter::{AudioFilter, Delay};

/// The shared block a CLAP filter's host process uses, however it is mapped: the
/// platform crate's OS mapping in the engine, a heap block in tests. Dropped on the
/// collector thread, never the mixer's.
pub type HostMemory = Arc<dyn AsRef<Shared> + Send + Sync>;

/// How long the switch between the plugin's output and the dry signal takes, ms.
const CROSSFADE_MS: f64 = 2.0;

/// A CLAP filter in a chain: the shared block's mixer side.
pub(crate) struct HostFilter {
    memory: HostMemory,
    /// Samples per block: `2 * block_frames`.
    block_samples: usize,
    depth: u64,
    /// The next block number to write.
    next: u64,
    /// The pipeline depth plus the plugin's latency, frames.
    latency: usize,
    /// The dry signal delayed by `latency`, used when the host's block is missing.
    dry: Delay,
    /// The host's block, read into here. `block_samples` long.
    wet: Box<[f32]>,
    /// Whether the last block was the host's output.
    was_wet: bool,
    /// The last frame this filter output, for the fade-out when the host goes missing.
    last: [f32; 2],
    /// Frames the crossfade takes: 1 to `block_frames`.
    fade_frames: usize,
}

impl std::fmt::Debug for HostFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostFilter")
            .field("depth", &self.depth)
            .field("latency", &self.latency)
            .field("next", &self.next)
            .field("was_wet", &self.was_wet)
            .finish_non_exhaustive()
    }
}

impl HostFilter {
    /// The mixer side of the host using `memory`, for a mixer at `rate` with blocks of
    /// `block_frames`. The block must be initialised for that rate and block size, and
    /// its host ready (it has loaded the plugin and reported its latency). Allocates:
    /// control thread only.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when the block isn't initialised for this mixer,
    /// the host isn't ready, or the latency would pass a chain's 100 ms.
    pub(crate) fn new(
        rate: u32,
        block_frames: usize,
        memory: HostMemory,
    ) -> Result<Self, AudioError> {
        let shared = (*memory).as_ref();
        let config = shared.config().ok_or(AudioError::InvalidConfig {
            what: "a CLAP filter's shared block must be initialised",
        })?;
        if config.sample_rate != rate || config.block_frames as usize != block_frames {
            return Err(AudioError::InvalidConfig {
                what: "a CLAP filter's shared block must match the mixer's rate and block size",
            });
        }
        let plugin_latency = shared.ready_latency().ok_or(AudioError::InvalidConfig {
            what: "a CLAP filter's host must be ready before the filter is added",
        })?;
        let latency = config.depth as usize * block_frames + plugin_latency as usize;
        let latency_ms = latency as f64 * 1000.0 / f64::from(rate);
        if latency_ms > f64::from(MAX_CHAIN_LATENCY_MS) {
            return Err(AudioError::InvalidConfig {
                what: "a CLAP filter's latency must be at most 100 ms",
            });
        }
        // Room for `latency` whatever the rounding, within the delay's 5000 ms.
        let mut dry = Delay::new(rate, (latency_ms + 1.0) as f32)?;
        dry.set_delay_frames(latency)?;
        // Within one block, so a switch never spans two.
        let fade_frames =
            ((CROSSFADE_MS * f64::from(rate) / 1000.0) as usize).clamp(1, block_frames);
        Ok(Self {
            block_samples: 2 * block_frames,
            depth: u64::from(config.depth),
            next: 0,
            latency,
            dry,
            wet: vec![0.0; 2 * block_frames].into_boxed_slice(),
            was_wet: false,
            last: [0.0; 2],
            fade_frames,
            memory,
        })
    }

    fn shared(&self) -> &Shared {
        (*self.memory).as_ref()
    }

    /// Reads the host's block `n − depth` into `wet`. False when it isn't there, or
    /// when `n` is still inside the pipeline's first `depth` blocks.
    fn read_wet(&mut self, n: u64) -> bool {
        let Some(want) = n.checked_sub(self.depth) else {
            return false;
        };
        let shared = (*self.memory).as_ref();
        shared.output.read(want, &mut self.wet)
    }
}

/// A sample, or 0 when it isn't finite: the host's output is another process's data.
fn finite(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

impl AudioFilter for HostFilter {
    fn process(&mut self, block: &mut [f32]) {
        let n = self.next;
        self.next = n.wrapping_add(1);
        let in_time = block.len() == self.block_samples;
        if in_time {
            self.shared().input.write(n, block);
        }
        // `block` becomes the dry signal, delayed by the filter's latency.
        self.dry.process(block);
        let wet = in_time && self.read_wet(n);
        let frames = block.len() / 2;
        let fade = self.fade_frames as f32;
        match (self.was_wet, wet) {
            (true, true) => {
                for (out, w) in block.iter_mut().zip(&self.wet) {
                    *out = finite(*w);
                }
            }
            (false, true) => {
                // Dry to the plugin's output, both known for this block.
                for i in 0..frames {
                    let g = ((i + 1) as f32 / fade).min(1.0);
                    for c in 0..2 {
                        let s = &mut block[2 * i + c];
                        *s = *s * (1.0 - g) + finite(self.wet[2 * i + c]) * g;
                    }
                }
            }
            (true, false) => {
                // The plugin's next block is missing: fade from its last frame to dry.
                for i in 0..frames {
                    let g = ((i + 1) as f32 / fade).min(1.0);
                    for c in 0..2 {
                        let s = &mut block[2 * i + c];
                        *s = *s * g + self.last[c] * (1.0 - g);
                    }
                }
            }
            (false, false) => {}
        }
        // The first `depth` blocks can't have arrived yet: not a miss.
        if !wet && n >= self.depth {
            let status = &self.shared().status;
            status.mixer_misses.fetch_add(1, Ordering::Relaxed);
            status.mixer_miss_run.fetch_add(1, Ordering::Relaxed);
        } else if wet && !self.was_wet {
            self.shared()
                .status
                .mixer_miss_run
                .store(0, Ordering::Relaxed);
        }
        self.was_wet = wet;
        if frames > 0 {
            self.last = [block[2 * frames - 2], block[2 * frames - 1]];
        }
    }

    fn latency_frames(&self) -> usize {
        self.latency
    }

    /// Clears the dry delay and the crossfade, and asks the host to reset the plugin.
    /// Block numbers carry on, so the rings stay in step.
    fn reset(&mut self) {
        self.dry.reset();
        self.was_wet = false;
        self.last = [0.0; 2];
        self.shared()
            .status
            .reset_seq
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_plugin_proto::{Config, first_useful};

    const RATE: u32 = 48_000;
    const FRAMES: usize = 64;

    fn memory(depth: u32, plugin_latency: Option<u32>) -> (Arc<Shared>, HostMemory) {
        let shared: Arc<Shared> = Arc::from(Shared::new_boxed());
        shared
            .init(Config {
                sample_rate: RATE,
                block_frames: FRAMES as u32,
                depth,
            })
            .unwrap();
        if let Some(latency) = plugin_latency {
            shared.publish_ready(latency);
        }
        let memory: HostMemory = Arc::new(Arc::clone(&shared));
        (shared, memory)
    }

    /// A host step done inline: process every block the mixer wrote that is still
    /// useful, multiplying by `gain`.
    fn host_step(shared: &Shared, next: &mut u64, gain: f32) {
        let head = shared.input.head();
        *next = (*next).max(first_useful(head, shared.config().unwrap().depth));
        let mut buf = vec![0.0; 2 * FRAMES];
        while *next < head {
            if shared.input.read(*next, &mut buf) {
                buf.iter_mut().for_each(|s| *s *= gain);
                shared.output.write(*next, &buf);
            }
            *next += 1;
        }
    }

    /// Block `n` of a test signal: every sample is `n + 1`.
    fn block(n: u64) -> Vec<f32> {
        vec![(n + 1) as f32; 2 * FRAMES]
    }

    #[test]
    fn refuses_blocks_not_ready_or_not_matching() {
        let (_, unready) = memory(2, None);
        assert!(HostFilter::new(RATE, FRAMES, unready).is_err());
        let (_, ready) = memory(2, Some(0));
        assert!(HostFilter::new(44_100, FRAMES, Arc::clone(&ready)).is_err());
        assert!(HostFilter::new(RATE, FRAMES * 2, Arc::clone(&ready)).is_err());
        assert!(HostFilter::new(RATE, FRAMES, ready).is_ok());
        let uninit: HostMemory = Arc::new(Shared::new_boxed());
        assert!(HostFilter::new(RATE, FRAMES, uninit).is_err());
        // 2 × 64 frames plus 4800 is over 100 ms at 48 kHz.
        let (_, slow) = memory(2, Some(4_800));
        assert!(HostFilter::new(RATE, FRAMES, slow).is_err());
    }

    #[test]
    fn latency_is_the_depth_plus_the_plugins() {
        let (_, mem) = memory(3, Some(100));
        let filter = HostFilter::new(RATE, FRAMES, mem).unwrap();
        assert_eq!(filter.latency_frames(), 3 * FRAMES + 100);
    }

    /// With a host that keeps up, the output is the host's output `depth` blocks late,
    /// and nothing counts as a miss.
    #[test]
    fn a_host_that_keeps_up_is_heard_depth_blocks_late() {
        let (shared, mem) = memory(2, Some(0));
        let mut filter = HostFilter::new(RATE, FRAMES, mem).unwrap();
        let mut host_next = 0;
        for n in 0..20u64 {
            let mut b = block(n);
            // The host ran during the previous block period.
            host_step(&shared, &mut host_next, 0.5);
            filter.process(&mut b);
            if n >= 3 {
                // Fully wet once the crossfade at block 2 is done.
                assert!(
                    b.iter().all(|&s| s == (n - 2 + 1) as f32 * 0.5),
                    "block {n}: {:?}",
                    &b[..4]
                );
            }
        }
        assert_eq!(shared.status.mixer_misses.load(Ordering::Relaxed), 0);
    }

    /// When the host stops, the output becomes the dry signal with the same latency,
    /// each missing block is counted, and the switch is a ramp from the last output.
    #[test]
    fn a_missing_host_is_bypassed_with_the_same_latency() {
        let (shared, mem) = memory(2, Some(0));
        let mut filter = HostFilter::new(RATE, FRAMES, mem).unwrap();
        let mut host_next = 0;
        let mut outs = Vec::new();
        for n in 0..10u64 {
            if n < 5 {
                host_step(&shared, &mut host_next, 1.0);
            }
            let mut b = block(n);
            filter.process(&mut b);
            outs.push(b);
        }
        // Blocks 0 and 1 are inside the pipeline. The host's last step, before block 4,
        // processed up to block 3, so blocks 2 to 5 read its output and 6 to 9 miss.
        let misses = shared.status.mixer_misses.load(Ordering::Relaxed);
        assert_eq!(misses, 4);
        assert_eq!(shared.status.mixer_miss_run.load(Ordering::Relaxed), 4);
        // With a gain of 1 the dry signal and the host's output agree, so the output
        // is the input two blocks late throughout, whichever path made it. Block 6, the
        // first miss, ramps there from block 5's last frame.
        for (n, b) in outs.iter().enumerate().skip(3) {
            let want = (n - 2 + 1) as f32;
            if n == 6 {
                assert!(b[0] > want - 1.0 && b[0] < want);
                assert_eq!(b[2 * FRAMES - 1], want);
                continue;
            }
            assert!(
                b.iter().all(|&s| (s - want).abs() < 1e-6),
                "block {n}: {:?}",
                &b[..4]
            );
        }
    }

    #[test]
    fn switching_crossfades_instead_of_jumping() {
        let (shared, mem) = memory(1, Some(0));
        let mut filter = HostFilter::new(RATE, FRAMES, mem).unwrap();
        let mut host_next = 0;
        // Host mutes (gain 0) for blocks 0..4, then goes missing.
        let mut outs = Vec::new();
        for n in 0..6u64 {
            if n < 4 {
                host_step(&shared, &mut host_next, 0.0);
            }
            let mut b = vec![1.0; 2 * FRAMES];
            filter.process(&mut b);
            outs.push(b);
        }
        // 2 ms is longer than a 64-frame block, so the fade takes the whole block.
        assert_eq!(filter.fade_frames, FRAMES);
        // Block 1 fades from dry (1.0) into the host's silence.
        let b1 = &outs[1];
        assert!(b1[0] < 1.0 && b1[0] > 0.9, "{}", b1[0]);
        assert_eq!(b1[2 * FRAMES - 1], 0.0);
        // Block 4 is the first miss: it fades from the host's last frame (0) to dry.
        let b4 = &outs[4];
        assert!(b4[0] > 0.0 && b4[0] < 0.1, "{}", b4[0]);
        assert!((b4[2 * FRAMES - 1] - 1.0).abs() < 1e-6);
        // No step between samples is larger than one fade step.
        let step = 1.0 / filter.fade_frames as f32 + 1e-4;
        for b in [b1, b4] {
            for w in b.chunks(2).collect::<Vec<_>>().windows(2) {
                assert!((w[1][0] - w[0][0]).abs() <= step);
            }
        }
    }

    #[test]
    fn non_finite_host_samples_become_silence() {
        let (shared, mem) = memory(1, Some(0));
        let mut filter = HostFilter::new(RATE, FRAMES, mem).unwrap();
        for n in 0..4u64 {
            let mut b = block(n);
            filter.process(&mut b);
            // The host answers block n with NaN, in time for the next block.
            assert!(shared.input.read(n, &mut vec![0.0; 2 * FRAMES]));
            shared.output.write(n, &vec![f32::NAN; 2 * FRAMES]);
            if n >= 2 {
                assert!(b.iter().all(|s| s.is_finite()));
            }
        }
        let mut b = block(4);
        filter.process(&mut b);
        assert!(b.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn reset_asks_the_host_to_reset_the_plugin() {
        let (shared, mem) = memory(2, Some(0));
        let mut filter = HostFilter::new(RATE, FRAMES, mem).unwrap();
        filter.reset();
        assert_eq!(shared.status.reset_seq.load(Ordering::Relaxed), 1);
    }
}
