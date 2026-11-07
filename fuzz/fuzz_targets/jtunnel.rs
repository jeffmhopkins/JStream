//! A datagram from the LAN, read as JStream reads JTunnel: a JTunnel packet, with its
//! frames' events walked, PCM decoded and SysEx joined, and an mDNS answer about JTunnel
//! senders.
//!
//! Neither may panic. An audio packet that parses writes back from its frames' raw
//! events and audio, and that parses to the same packet.

#![no_main]

use jstream_jtunnel::wire::{self, Event, FrameOut, Packet};
use jstream_jtunnel::{SeqTracker, SysexAssembler, dnssd};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = dnssd::parse_announcements(data);

    let Ok(packet) = wire::parse(data) else {
        return;
    };
    let Packet::Audio { header, frames } = packet else {
        return;
    };
    let mut seq = SeqTracker::new();
    let mut sysex = SysexAssembler::new();
    let mut samples = [0.0_f32; 2 * wire::MAX_FRAME_SAMPLES as usize];
    let mut raw = Vec::new();
    for f in frames.iter().rev().flatten() {
        let _ = seq.frame(header.stream_id, f.seq);
        wire::decode_pcm24(f.audio, &mut samples);
        let mut count = 0;
        for (offset, e) in f.events() {
            assert!(offset < header.frame_samples);
            if let Event::SysexPiece(p) = e {
                let _ = sysex.push(&p);
            }
            count += 1;
        }
        assert_eq!(count, f.event_count());
    }
    for f in frames.iter().flatten() {
        raw.push(f.raw_events().collect::<Vec<_>>());
    }
    let outs: Vec<FrameOut<'_>> = frames
        .iter()
        .flatten()
        .zip(&raw)
        .map(|(f, events)| FrameOut {
            events,
            audio: f.audio,
        })
        .collect();
    let mut buf = [0; 4096];
    let n = wire::write_audio(&mut buf, &header, &outs).expect("a parsed packet writes");
    assert_eq!(wire::parse(&buf[..n]), Ok(packet));
});
