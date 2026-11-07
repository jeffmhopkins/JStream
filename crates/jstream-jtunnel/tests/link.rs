//! A sender and a receiver whose clocks run 150 ppm apart, joined by a simulated Wi-Fi
//! link with jitter, 60 ms stalls and 2% loss, for 90 simulated seconds, with
//! redundancy on: the shape of JTunnel's own engine test. The receiver must find the
//! drift, hand the mixer every frame in order with smooth times, and, once settled,
//! conceal only the frames whose packet and the next one (its redundant copy) were both
//! lost.

use jstream_jtunnel::wire::{self, AudioHeader, Codec, FrameOut, Packet};
use jstream_jtunnel::{Playout, Pushed};

struct Lcg(u64);
impl Lcg {
    fn unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (self.0 >> 11) as f64 / (1_u64 << 53) as f64
    }
}

const FRAME: u16 = 96; // 2 ms PCM at 48 kHz
const MS: u64 = 1_000_000;

#[test]
fn a_drifting_lossy_link_plays_every_frame_in_order() {
    let mut rng = Lcg(42);
    let ppm = 150.0;
    let mut playout = Playout::new(90 * MS);
    let pcm = [0_u8; wire::MAX_FRAME_BYTES];
    let samples = [0.25_f32; 2 * FRAME as usize];
    let frames: u32 = 45_000; // 90 s
    // Each packet's arrival time, by the receiver's clock.
    let mut arrivals: Vec<(u64, u32)> = Vec::new();
    let mut stall_until = 0.0_f64;
    let mut lost = vec![false; frames as usize];
    for seq in 0..frames {
        let sent = f64::from(seq + 1) * 0.002 * (1.0 + ppm / 1e6);
        if rng.unit() < 0.02 {
            lost[seq as usize] = true;
            continue;
        }
        // About every 10 s, everything stops for 60 ms and goes out in a burst.
        if rng.unit() < 0.002 / 10.0 * 1.0 && sent > stall_until {
            stall_until = sent + 0.060;
        }
        let jitter = 0.006 * rng.unit().powi(2);
        let at = sent.max(stall_until) + 0.002 + jitter;
        arrivals.push(((at * 1e9) as u64, seq));
    }
    arrivals.sort_unstable();

    let mut buf = [0_u8; wire::MAX_PACKET_BYTES];
    let mut next_out = 0_u64;
    let mut last_time: Option<u64> = None;
    let mut concealed_late = 0;
    let mut out_frames = 0;
    let mut tick = 0_u64;
    let mut first = true;
    let mut i = 0;
    while i < arrivals.len() || tick < arrivals.last().map_or(0, |a| a.0) + 200 * MS {
        tick += MS;
        while i < arrivals.len() && arrivals[i].0 <= tick {
            let (at, seq) = arrivals[i];
            i += 1;
            let header = AudioHeader {
                stream_id: 9,
                seq,
                sample_rate: 48_000,
                codec: Codec::Pcm24,
                frame_samples: FRAME,
            };
            let f = FrameOut {
                events: &[],
                audio: &pcm[..usize::from(FRAME) * 6],
            };
            let n = wire::write_audio(&mut buf, &header, &[f, f][..if seq > 0 { 2 } else { 1 }])
                .unwrap_or(0);
            let Ok(Packet::Audio { header, frames }) = wire::parse(&buf[..n]) else {
                panic!("bad packet");
            };
            for frame in frames.iter().rev().flatten() {
                let r = playout.push(&header, frame, &samples, at);
                assert!(r != Pushed::Restarted || first, "only one stream");
                first = false;
            }
        }
        while let Some(d) = playout.pop(tick) {
            if next_out == u64::from(frames) {
                break; // past the end, the stream is silence
            }
            assert_eq!(d.position, next_out * u64::from(FRAME), "in order, no gaps");
            if let Some(prev) = last_time {
                let step = d.time_ns as i64 - prev as i64;
                // 2 ms a frame, give or take the drift correction.
                assert!(
                    (1_900_000..2_100_000).contains(&step),
                    "step {step} at {next_out}"
                );
            }
            let n = next_out as usize;
            let gone = lost[n] && lost.get(n + 1).copied().unwrap_or(true);
            if next_out > 10_000 {
                assert_eq!(d.concealed, gone, "frame {next_out}: {:?}", playout.stats());
                concealed_late += usize::from(gone);
            }
            last_time = Some(d.time_ns);
            next_out += 1;
            out_frames += 1;
        }
    }
    let drift = playout.clock().drift_ppm().unwrap_or(0.0);
    assert!((drift - ppm).abs() < 20.0, "drift {drift}");
    assert!(out_frames >= frames - 1, "played {out_frames} of {frames}");
    // Both copies of a frame go missing about one time in 2 500.
    assert!(
        concealed_late < 40,
        "{concealed_late} concealed after settling"
    );
}
