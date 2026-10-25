//! The encoder doesn't allocate in steady state: once its buffers have grown to the
//! sizes a stream uses, pushing a buffer and encoding the frames it completes allocate
//! nothing (R-RT-002: no per-item allocation in per-frame loops).
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator, which
//! counts every allocation made inside an `assert_no_alloc` scope.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use jstream_aac::{Config, Encoder, WindowShape};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn steady_state_encoding_does_not_allocate() {
    // The counter works: an allocation inside a scope is seen. (One test, so no other
    // test's scope can move the global counter while this one measures.)
    let before = violation_count();
    let v = assert_no_alloc(|| std::hint::black_box(vec![0u8; 64]));
    assert!(violation_count() > before);
    drop(v);

    // The panned stream's right channel is the left one at -8 dB, so intensity stereo is
    // exercised too.
    for (channels, shape, panned) in [
        (2u8, WindowShape::Sine, false),
        (1, WindowShape::Kbd, false),
        (2, WindowShape::Sine, true),
    ] {
        let config = Config {
            window_shape: shape,
            ..Config::new(48_000, channels, 64_000 * u32::from(channels))
        };
        let mut enc = Encoder::new(config).expect("valid config");
        let mut au = Vec::with_capacity(6144 / 8 * usize::from(channels));
        let frames = 960; // 20 ms, as the mixer delivers it
        let mut seed = 1u32;
        let mut buffer = vec![0.0f32; frames * usize::from(channels)];
        let mut fill = |buf: &mut [f32], tick: usize| {
            for (i, s) in buf.iter_mut().enumerate() {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f32 / (1 << 24) as f32 - 0.5;
                // Bursts every half second, so short windows are exercised too.
                let burst = if tick.is_multiple_of(25) && i < 400 {
                    0.9
                } else {
                    0.05
                };
                *s = noise * burst;
            }
            if panned {
                for f in buf.chunks_exact_mut(2) {
                    f[1] = 0.4 * f[0];
                }
            }
        };
        // Warm up: the input buffers reach their working size.
        for tick in 0..50 {
            fill(&mut buffer, tick);
            enc.push_interleaved(&buffer).expect("whole frames");
            while enc.encode_frame(&mut au) {
                au.clear();
            }
        }
        let before = violation_count();
        for tick in 50..300 {
            fill(&mut buffer, tick);
            assert_no_alloc(|| {
                enc.push_interleaved(&buffer).expect("whole frames");
                while enc.encode_frame(&mut au) {
                    au.clear();
                }
            });
        }
        assert_eq!(violation_count(), before, "{channels} channels allocated");
    }
}
