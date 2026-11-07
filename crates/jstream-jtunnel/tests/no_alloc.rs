//! Parsing a packet, walking its events, joining SysEx and tracking sequence numbers
//! allocate nothing, so the receive thread can run them on every datagram.
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator, which
//! counts every allocation made inside an `assert_no_alloc` scope.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use jstream_jtunnel::wire::{self, AudioHeader, Codec, Event, FrameOut, Packet};
use jstream_jtunnel::{SeqTracker, SysexAssembler};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn receiving_a_packet_does_not_allocate() {
    let before = violation_count();
    let v = assert_no_alloc(|| std::hint::black_box(vec![0u8; 64]));
    assert!(violation_count() > before);
    drop(v);

    let pcm = [0_u8; wire::MAX_FRAME_BYTES];
    let events: [(u16, &[u8]); 3] = [
        (0, &[0x90, 60, 100]),
        (3, &[1, 1, 0, 2, 0xF0, 1]),
        (4, &[1, 2, 1, 1, 0xF7]),
    ];
    let frame = FrameOut {
        events: &events,
        audio: &pcm,
    };
    let header = AudioHeader {
        stream_id: 1,
        seq: 1,
        sample_rate: 48_000,
        codec: Codec::Pcm24,
        frame_samples: 96,
    };
    let mut buf = [0; wire::MAX_PACKET_BYTES];
    let n = wire::write_audio(&mut buf, &header, &[frame, frame]).unwrap_or(0);
    assert!(n > 0);

    let mut seq = SeqTracker::new();
    let mut sysex = SysexAssembler::new();
    let mut samples = [0.0_f32; 192];
    let mut whole = 0;
    let before = violation_count();
    assert_no_alloc(|| {
        for _ in 0..100 {
            let Ok(Packet::Audio { header, frames }) = wire::parse(&buf[..n]) else {
                continue;
            };
            for f in frames.iter().rev().flatten() {
                let _ = seq.frame(header.stream_id, f.seq);
                wire::decode_pcm24(f.audio, &mut samples);
                for (_, e) in f.events() {
                    if let Event::SysexPiece(p) = e
                        && sysex.push(&p).is_some()
                    {
                        whole += 1;
                    }
                }
            }
        }
    });
    assert_eq!(violation_count(), before);
    assert_eq!(whole, 200);
}
