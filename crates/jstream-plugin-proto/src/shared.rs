//! The whole shared block and its configuration.

use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::{Ordering, fence};

use crate::status::{STATE_EMPTY, STATE_READY, Status};
use crate::{
    BlockRing, CHANNELS, MAGIC, MAX_BLOCK_FRAMES, MAX_DEPTH, MAX_PLUGIN_LATENCY, ParamRing, VERSION,
};

/// Everything one CLAP filter instance shares with its host process, at offset 0 of the
/// mapping. The mapping is exactly `size_of::<Shared>()` bytes.
#[repr(C)]
#[derive(Debug, Default)]
pub struct Shared {
    /// Configuration, the host's state and both sides' counters.
    pub status: Status,
    /// The mixer's dry blocks, written by the mixer and read by the host.
    pub input: BlockRing,
    /// The processed blocks, written by the host and read by the mixer.
    pub output: BlockRing,
    /// Parameter changes from the engine (the filter's settings) to the plugin.
    pub params_in: ParamRing,
    /// Parameter changes the plugin made itself (its own editor), back to the engine.
    pub params_out: ParamRing,
}

// The layout is the contract: any change here is a VERSION bump.
const _: () = {
    assert!(align_of::<Shared>() == 8);
    assert!(offset_of!(Shared, status) == 0);
    assert!(offset_of!(Shared, input) == size_of::<Status>());
    assert!(offset_of!(Shared, output) == offset_of!(Shared, input) + size_of::<BlockRing>());
    assert!(offset_of!(Shared, params_in) == offset_of!(Shared, output) + size_of::<BlockRing>());
    assert!(
        offset_of!(Shared, params_out) == offset_of!(Shared, params_in) + size_of::<ParamRing>()
    );
    assert!(
        size_of::<Shared>()
            == size_of::<Status>() + 2 * size_of::<BlockRing>() + 2 * size_of::<ParamRing>()
    );
};

/// How the engine runs one CLAP filter: what the host activates the plugin with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The engine's sample rate, Hz, 8000 to 192000.
    pub sample_rate: u32,
    /// Frames per block, 1 to [`MAX_BLOCK_FRAMES`].
    pub block_frames: u32,
    /// The pipeline depth, blocks, 1 to [`MAX_DEPTH`].
    pub depth: u32,
}

/// Which [`Config`] field is out of range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// `sample_rate` is outside 8000 to 192000.
    SampleRate,
    /// `block_frames` is 0 or over [`MAX_BLOCK_FRAMES`].
    BlockFrames,
    /// `depth` is 0 or over [`MAX_DEPTH`].
    Depth,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SampleRate => "sample rate must be 8000..=192000",
            Self::BlockFrames => "block frames must be 1..=2048",
            Self::Depth => "pipeline depth must be 1..=6",
        })
    }
}

// Written out: this crate has no dependencies.
impl std::error::Error for ConfigError {}

impl Config {
    /// Checks every field's range.
    ///
    /// # Errors
    /// The first field out of range.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(8_000..=192_000).contains(&self.sample_rate) {
            return Err(ConfigError::SampleRate);
        }
        if !(1..=MAX_BLOCK_FRAMES as u32).contains(&self.block_frames) {
            return Err(ConfigError::BlockFrames);
        }
        if !(1..=MAX_DEPTH).contains(&self.depth) {
            return Err(ConfigError::Depth);
        }
        Ok(())
    }
}

/// A block on the heap is its own block: so a test, or a host of the block that isn't
/// an OS mapping, can hand an `Arc<Shared>` to code that takes any `AsRef<Shared>`.
impl AsRef<Shared> for Shared {
    fn as_ref(&self) -> &Shared {
        self
    }
}

impl Shared {
    /// A zeroed block on the heap, as a fresh mapping would be. For tests and for hosts
    /// of the block that aren't an OS mapping.
    pub fn new_boxed() -> Box<Self> {
        Box::default()
    }

    /// Initialises the block for a new host with `config`: unpublishes it, empties both
    /// rings and both parameter rings, zeroes every counter and state, writes the
    /// configuration, then publishes it. The engine's side, only while no host has the
    /// block open.
    ///
    /// # Errors
    /// A field of `config` out of range; the block is left unpublished.
    pub fn init(&self, config: Config) -> Result<(), ConfigError> {
        let s = &self.status;
        s.magic.store(0, Ordering::Relaxed);
        fence(Ordering::Release);
        config.validate()?;
        self.input.clear();
        self.output.clear();
        self.params_in.clear();
        self.params_out.clear();
        s.sample_rate.store(config.sample_rate, Ordering::Relaxed);
        s.block_frames.store(config.block_frames, Ordering::Relaxed);
        s.depth.store(config.depth, Ordering::Relaxed);
        s.channels.store(CHANNELS as u32, Ordering::Relaxed);
        s.host_state.store(STATE_EMPTY, Ordering::Relaxed);
        for field in [&s.host_error, &s.plugin_latency, &s.stop] {
            field.store(0, Ordering::Relaxed);
        }
        for field in [
            &s.reset_seq,
            &s.host_reset_seq,
            &s.heartbeat,
            &s.processed,
            &s.host_skipped,
            &s.mixer_misses,
            &s.mixer_miss_run,
        ] {
            field.store(0, Ordering::Relaxed);
        }
        s.version.store(VERSION, Ordering::Relaxed);
        s.magic.store(MAGIC, Ordering::Release);
        Ok(())
    }

    /// The configuration, when the block is initialised by an engine of this
    /// [`VERSION`] and every field is in range. The host's first read; `None` means it
    /// must not use the block.
    pub fn config(&self) -> Option<Config> {
        let s = &self.status;
        if s.magic.load(Ordering::Acquire) != MAGIC
            || s.version.load(Ordering::Relaxed) != VERSION
            || s.channels.load(Ordering::Relaxed) != CHANNELS as u32
        {
            return None;
        }
        let config = Config {
            sample_rate: s.sample_rate.load(Ordering::Relaxed),
            block_frames: s.block_frames.load(Ordering::Relaxed),
            depth: s.depth.load(Ordering::Relaxed),
        };
        config.validate().ok().map(|()| config)
    }

    /// The host's side: records the plugin's latency, then marks the host ready.
    pub fn publish_ready(&self, plugin_latency: u32) {
        self.status
            .plugin_latency
            .store(plugin_latency, Ordering::Relaxed);
        self.status.host_state.store(STATE_READY, Ordering::Release);
    }

    /// The plugin's latency, frames, once the host is ready and the latency it reported
    /// is at most [`MAX_PLUGIN_LATENCY`]. `None` before that, or for a host that failed.
    pub fn ready_latency(&self) -> Option<u32> {
        if self.status.host_state.load(Ordering::Acquire) != STATE_READY {
            return None;
        }
        let latency = self.status.plugin_latency.load(Ordering::Relaxed);
        (latency <= MAX_PLUGIN_LATENCY).then_some(latency)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_DEPTH, ParamEvent, STATE_FAILED, first_useful};
    use std::sync::Arc;

    const CONFIG: Config = Config {
        sample_rate: 48_000,
        block_frames: 256,
        depth: DEFAULT_DEPTH,
    };

    #[test]
    fn a_fresh_block_has_no_config() {
        let shared = Shared::new_boxed();
        assert_eq!(shared.config(), None);
        assert_eq!(shared.ready_latency(), None);
    }

    #[test]
    fn init_publishes_the_config() {
        let shared = Shared::new_boxed();
        shared.init(CONFIG).unwrap();
        assert_eq!(shared.config(), Some(CONFIG));
        // A foreign version is refused.
        shared.status.version.store(VERSION + 1, Ordering::Relaxed);
        assert_eq!(shared.config(), None);
    }

    #[test]
    fn init_refuses_bad_configs_and_unpublishes() {
        let shared = Shared::new_boxed();
        shared.init(CONFIG).unwrap();
        let bad = [
            (
                Config {
                    sample_rate: 7_999,
                    ..CONFIG
                },
                ConfigError::SampleRate,
            ),
            (
                Config {
                    block_frames: 0,
                    ..CONFIG
                },
                ConfigError::BlockFrames,
            ),
            (
                Config {
                    block_frames: MAX_BLOCK_FRAMES as u32 + 1,
                    ..CONFIG
                },
                ConfigError::BlockFrames,
            ),
            (Config { depth: 0, ..CONFIG }, ConfigError::Depth),
            (
                Config {
                    depth: MAX_DEPTH + 1,
                    ..CONFIG
                },
                ConfigError::Depth,
            ),
        ];
        for (config, err) in bad {
            assert_eq!(shared.init(config), Err(err));
            assert_eq!(shared.config(), None);
        }
    }

    #[test]
    fn init_clears_what_a_previous_host_left() {
        let shared = Shared::new_boxed();
        shared.init(CONFIG).unwrap();
        assert!(shared.input.write(0, &[0.5; 512]));
        assert!(shared.params_in.push(ParamEvent { id: 1, value: 1.0 }));
        shared.publish_ready(64);
        shared.status.mixer_misses.store(9, Ordering::Relaxed);
        shared.init(CONFIG).unwrap();
        assert_eq!(shared.input.head(), 0);
        assert_eq!(shared.params_in.pop(), None);
        assert_eq!(shared.ready_latency(), None);
        assert_eq!(shared.status.mixer_misses.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ready_latency_needs_a_ready_host_in_range() {
        let shared = Shared::new_boxed();
        shared.init(CONFIG).unwrap();
        shared.publish_ready(480);
        assert_eq!(shared.ready_latency(), Some(480));
        shared.publish_ready(MAX_PLUGIN_LATENCY + 1);
        assert_eq!(shared.ready_latency(), None);
        shared
            .status
            .host_state
            .store(STATE_FAILED, Ordering::Release);
        assert_eq!(shared.ready_latency(), None);
    }

    /// A host thread that halves every sample, against a mixer loop that never waits:
    /// once the pipeline has filled, every block the mixer reads is the host's output
    /// for the block `depth` earlier.
    #[test]
    fn a_host_thread_processes_blocks_depth_behind() {
        let shared: Arc<Shared> = Arc::from(Shared::new_boxed());
        shared.init(CONFIG).unwrap();
        let frames = CONFIG.block_frames as usize;
        let host = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let config = shared.config().unwrap();
                shared.publish_ready(0);
                let mut buf = vec![0.0; frames * CHANNELS];
                let mut next = 0u64;
                while shared.status.stop.load(Ordering::Acquire) == 0 {
                    let head = shared.input.head();
                    next = next.max(first_useful(head, config.depth));
                    if next < head && shared.input.read(next, &mut buf) {
                        buf.iter_mut().for_each(|s| *s *= 0.5);
                        shared.output.write(next, &buf);
                        shared.status.processed.fetch_add(1, Ordering::Relaxed);
                        next += 1;
                    } else {
                        std::thread::yield_now();
                    }
                }
            })
        };
        while shared.ready_latency().is_none() {
            std::thread::yield_now();
        }
        let depth = u64::from(CONFIG.depth);
        let mut out = vec![0.0; frames * CHANNELS];
        let mut hits = 0;
        for n in 0..200u64 {
            let block = vec![n as f32; frames * CHANNELS];
            assert!(shared.input.write(n, &block));
            // Give the host the time a real engine block would.
            std::thread::sleep(std::time::Duration::from_micros(200));
            if n >= depth && shared.output.read(n - depth, &mut out) {
                hits += 1;
                assert!(out.iter().all(|&s| s == (n - depth) as f32 * 0.5));
            }
        }
        shared.status.stop.store(1, Ordering::Release);
        host.join().unwrap();
        // Timing isn't guaranteed on a loaded CI machine, so only some blocks must
        // arrive; every block that did was the right one.
        assert!(hits > 0, "the host produced no block in time");
    }
}
