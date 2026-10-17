//! The host's real-time loop: on every pass it beats the heartbeat, applies a reset the
//! mixer asked for, and runs the plugin over every input block the mixer can still read,
//! writing each result under the same block number. It never allocates, locks or waits
//! on the engine (R-RT-001); between passes it sleeps a fraction of a block.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jstream_plugin_clap::{MAX_BLOCK_EVENTS, Processor};
use jstream_plugin_proto::{CHANNELS, Config, ParamEvent, Shared, first_useful};

/// What the loop runs: the plugin's processor, or a stand-in in tests.
pub(crate) trait Effect {
    /// Processes one interleaved stereo block with `params` applied at its start, and
    /// returns the parameter changes the effect made itself. Never allocates.
    fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        params: &[ParamEvent],
    ) -> Result<&[ParamEvent], ()>;

    /// Clears the effect's audio state.
    fn reset(&mut self);
}

impl Effect for Processor {
    fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        params: &[ParamEvent],
    ) -> Result<&[ParamEvent], ()> {
        Processor::process(self, input, output, params).map_err(|_| ())
    }

    fn reset(&mut self) {
        Processor::reset(self);
    }
}

/// The loop's own state and buffers, all sized when it is made.
pub(crate) struct Rt {
    depth: u32,
    /// The next block to process.
    next: u64,
    input: Vec<f32>,
    output: Vec<f32>,
    /// The parameter changes for the block being processed.
    params: Vec<ParamEvent>,
}

impl Rt {
    pub(crate) fn new(config: Config) -> Self {
        let samples = config.block_frames as usize * CHANNELS;
        Self {
            depth: config.depth,
            next: 0,
            input: vec![0.0; samples],
            output: vec![0.0; samples],
            params: Vec::with_capacity(MAX_BLOCK_EVENTS),
        }
    }

    /// One pass over the block; returns the blocks it processed. Never allocates.
    pub(crate) fn pass(&mut self, shared: &Shared, effect: &mut impl Effect) -> usize {
        let s = &shared.status;
        s.heartbeat.fetch_add(1, Ordering::Release);
        let reset = s.reset_seq.load(Ordering::Acquire);
        if reset != s.host_reset_seq.load(Ordering::Relaxed) {
            effect.reset();
            s.host_reset_seq.store(reset, Ordering::Release);
        }
        // The head is the mixer's word: anything it claims, the reads below check.
        let head = shared.input.head();
        // A host that fell behind skips what the mixer has already passed.
        let skip_to = first_useful(head, self.depth);
        if skip_to > self.next {
            s.host_skipped
                .fetch_add(skip_to - self.next, Ordering::Relaxed);
            self.next = skip_to;
        }
        let mut done = 0;
        while self.next < head {
            let block = self.next;
            self.next += 1;
            if !shared.input.read(block, &mut self.input) {
                s.host_skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            self.params.clear();
            while self.params.len() < self.params.capacity() {
                let Some(event) = shared.params_in.pop() else {
                    break;
                };
                self.params.push(event);
            }
            match effect.process(&self.input, &mut self.output, &self.params) {
                Ok(reported) => {
                    // A full ring drops the rest: the values are re-read on the main
                    // thread whenever the engine asks.
                    for event in reported {
                        if !shared.params_out.push(*event) {
                            break;
                        }
                    }
                    shared.output.write(block, &self.output);
                    s.processed.fetch_add(1, Ordering::Release);
                    done += 1;
                }
                // Nothing written: the mixer bypasses the block.
                Err(()) => {
                    s.host_skipped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        done
    }
}

/// How long the loop sleeps between passes: a quarter of a block, from 0.5 to 5 ms.
pub(crate) fn period(config: Config) -> Duration {
    let block = Duration::from_secs_f64(
        f64::from(config.block_frames) / f64::from(config.sample_rate.max(1)),
    );
    (block / 4).clamp(Duration::from_micros(500), Duration::from_millis(5))
}

/// Runs passes until `stop` is set or the engine sets the block's `stop` word, then
/// drops `effect` here, on this thread (a CLAP processor stops processing on its audio
/// thread).
pub(crate) fn run<E: Effect>(shared: &Shared, config: Config, effect: E, stop: &AtomicBool) {
    let mut effect = effect;
    let mut rt = Rt::new(config);
    let period = period(config);
    while !stop.load(Ordering::Acquire) && shared.status.stop.load(Ordering::Acquire) == 0 {
        rt.pass(shared, &mut effect);
        std::thread::sleep(period);
    }
    drop(effect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_plugin_proto::DEFAULT_DEPTH;

    const CONFIG: Config = Config {
        sample_rate: 48_000,
        block_frames: 64,
        depth: DEFAULT_DEPTH,
    };

    /// Doubles the signal; echoes each parameter change with id + 100.
    #[derive(Default)]
    struct Double {
        resets: u32,
        echo: Vec<ParamEvent>,
        fail: bool,
    }

    impl Effect for Double {
        fn process(
            &mut self,
            input: &[f32],
            output: &mut [f32],
            params: &[ParamEvent],
        ) -> Result<&[ParamEvent], ()> {
            if self.fail {
                return Err(());
            }
            for (o, i) in output.iter_mut().zip(input) {
                *o = i * 2.0;
            }
            self.echo.clear();
            self.echo.extend(params.iter().map(|p| ParamEvent {
                id: p.id + 100,
                value: p.value,
            }));
            Ok(&self.echo)
        }

        fn reset(&mut self) {
            self.resets += 1;
        }
    }

    fn block(n: u64) -> Vec<f32> {
        vec![n as f32; CONFIG.block_frames as usize * CHANNELS]
    }

    fn shared() -> Box<Shared> {
        let shared = Shared::new_boxed();
        shared.init(CONFIG).unwrap();
        shared
    }

    #[test]
    fn processes_each_new_block_under_its_number() {
        let shared = shared();
        let mut rt = Rt::new(CONFIG);
        let mut fx = Double {
            echo: Vec::with_capacity(MAX_BLOCK_EVENTS),
            ..Double::default()
        };
        assert_eq!(rt.pass(&shared, &mut fx), 0);
        for n in 0..2 {
            assert!(shared.input.write(n, &block(n)));
        }
        shared.params_in.push(ParamEvent { id: 1, value: 0.5 });
        assert_eq!(rt.pass(&shared, &mut fx), 2);
        assert!(shared.input.write(2, &block(2)));
        assert_eq!(rt.pass(&shared, &mut fx), 1);
        let mut out = block(0);
        for n in 0..3 {
            assert!(shared.output.read(n, &mut out));
            assert_eq!(out, block(n * 2));
        }
        // The change went in with the first block, and its echo came back.
        assert_eq!(
            shared.params_out.pop(),
            Some(ParamEvent {
                id: 101,
                value: 0.5
            })
        );
        assert_eq!(shared.params_out.pop(), None);
        let s = &shared.status;
        assert_eq!(s.processed.load(Ordering::Relaxed), 3);
        assert_eq!(s.heartbeat.load(Ordering::Relaxed), 3);
        assert_eq!(s.host_skipped.load(Ordering::Relaxed), 0);
        // Nothing new: nothing done.
        assert_eq!(rt.pass(&shared, &mut fx), 0);
    }

    #[test]
    fn a_host_behind_skips_what_the_mixer_already_passed() {
        let shared = shared();
        let mut rt = Rt::new(CONFIG);
        let mut fx = Double::default();
        for n in 0..6 {
            shared.input.write(n, &block(n));
        }
        // Blocks 0 to 3 are past reading (the mixer is at block 6 and reads 4).
        assert_eq!(rt.pass(&shared, &mut fx), 2);
        assert_eq!(shared.status.host_skipped.load(Ordering::Relaxed), 4);
        let mut out = block(0);
        assert!(!shared.output.read(3, &mut out));
        assert!(shared.output.read(5, &mut out));
    }

    #[test]
    fn resets_once_per_request_and_skips_failed_blocks() {
        let shared = shared();
        let mut rt = Rt::new(CONFIG);
        let mut fx = Double::default();
        shared.status.reset_seq.store(1, Ordering::Release);
        rt.pass(&shared, &mut fx);
        rt.pass(&shared, &mut fx);
        assert_eq!(fx.resets, 1);
        assert_eq!(shared.status.host_reset_seq.load(Ordering::Relaxed), 1);
        fx.fail = true;
        shared.input.write(0, &block(0));
        assert_eq!(rt.pass(&shared, &mut fx), 0);
        let mut out = block(0);
        assert!(!shared.output.read(0, &mut out));
        assert_eq!(shared.status.host_skipped.load(Ordering::Relaxed), 1);
        // A block of the wrong size (a block re-initialised under it) is skipped too.
        fx.fail = false;
        shared.input.write(1, &[0.0; 4]);
        assert_eq!(rt.pass(&shared, &mut fx), 0);
        assert_eq!(shared.status.host_skipped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_pass_does_not_allocate() {
        let shared = shared();
        let mut rt = Rt::new(CONFIG);
        let mut fx = Double {
            echo: Vec::with_capacity(MAX_BLOCK_EVENTS),
            ..Double::default()
        };
        for n in 0..2 {
            shared.input.write(n, &block(n));
        }
        for i in 0..(MAX_BLOCK_EVENTS as u32 + 10) {
            shared.params_in.push(ParamEvent { id: i, value: 1.0 });
        }
        let before = assert_no_alloc::violation_count();
        let done = assert_no_alloc::assert_no_alloc(|| rt.pass(&shared, &mut fx));
        assert_eq!(done, 2);
        assert_eq!(assert_no_alloc::violation_count(), before);
    }

    #[test]
    fn a_pass_over_a_real_plugin_does_not_allocate() {
        use jstream_plugin_clap::{Plugin, test_plugin};
        let library = test_plugin::library().unwrap();
        let mut plugin = Plugin::new(&library, test_plugin::GAIN_ID).unwrap();
        let mut processor = plugin
            .activate(CONFIG.sample_rate, CONFIG.block_frames)
            .unwrap();
        let shared = shared();
        let mut rt = Rt::new(CONFIG);
        // The first block starts processing, which the plugin may allocate for.
        shared.input.write(0, &block(1));
        assert_eq!(rt.pass(&shared, &mut processor), 1);
        shared.input.write(1, &block(1));
        shared.params_in.push(ParamEvent {
            id: test_plugin::GAIN_PARAM,
            value: 0.5,
        });
        let before = assert_no_alloc::violation_count();
        let done = assert_no_alloc::assert_no_alloc(|| rt.pass(&shared, &mut processor));
        assert_eq!(done, 1);
        assert_eq!(assert_no_alloc::violation_count(), before);
        let mut out = block(0);
        assert!(shared.output.read(1, &mut out));
        assert!(out.iter().all(|s| *s == 0.5));
        drop(processor);
        plugin.deactivate().unwrap();
    }

    #[test]
    fn stops_on_the_engines_word_and_paces_itself() {
        let shared = shared();
        shared.status.stop.store(1, Ordering::Release);
        run(&shared, CONFIG, Double::default(), &AtomicBool::new(false));
        assert_eq!(period(CONFIG), Duration::from_micros(500));
        let slow = Config {
            block_frames: 2048,
            sample_rate: 8_000,
            ..CONFIG
        };
        assert_eq!(period(slow), Duration::from_millis(5));
    }
}
