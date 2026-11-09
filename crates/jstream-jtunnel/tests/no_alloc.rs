//! Parsing a packet, walking its events, joining SysEx, tracking sequence numbers and
//! playing frames out allocate nothing, so the receive thread can run them on every datagram.
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator, which
//! counts every allocation made inside an `assert_no_alloc` scope.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use jstream_jtunnel::wire::{self, AudioHeader, Codec, Event, FrameOut, Packet};
use jstream_jtunnel::{Playout, SeqTracker, SysexAssembler};

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
    let mut playout = Playout::new(20_000_000);
    let mut played = 0;
    let mut packets = [[0_u8; wire::MAX_PACKET_BYTES]; 4];
    let mut lens = [0; 4];
    for (k, p) in packets.iter_mut().enumerate() {
        let h = AudioHeader {
            seq: k as u32 + 2,
            ..header
        };
        lens[k] = wire::write_audio(p, &h, &[frame, frame]).unwrap_or(0);
    }
    let before = violation_count();
    assert_no_alloc(|| {
        for k in 0..400_u64 {
            let p = &packets[(k % 4) as usize];
            let Ok(Packet::Audio { header, frames }) = wire::parse(&p[..lens[(k % 4) as usize]])
            else {
                continue;
            };
            for f in frames.iter().rev().flatten() {
                wire::decode_pcm24(f.audio, &mut samples);
                let _ = playout.push(&header, f, &samples, 1_000_000_000 + k * 2_000_000);
            }
            while let Some(d) = playout.pop(1_000_000_000 + k * 2_000_000) {
                played += d.events().count();
            }
        }
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
    assert!(played > 0);
}

/// Review B1 on #1447: an mDNS answer about anything but a JTunnel instance, however
/// its records (at most 256) repeat however long a name, is read without allocating, so a LAN host's
/// datagram to the receive socket costs the playout thread no allocations.
#[test]
fn an_mdns_answer_about_other_names_does_not_allocate() {
    use jstream_jtunnel::dnssd::parse_announcements;
    // 126 one-byte labels (the most a pointed-to name may have), then records that each point back at that name.
    let mut m = vec![0, 0, 0x84, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let name_at = m.len();
    for _ in 0..126 {
        m.extend_from_slice(&[1, b'a']);
    }
    m.push(0);
    let srv = |m: &mut Vec<u8>| {
        m.extend_from_slice(&[0, 33, 0, 1, 0, 0, 0, 30, 0, 6, 0, 0, 0, 0, 0x23, 0x28]);
    };
    srv(&mut m);
    let mut records = 1_u16;
    // As many records as a message may declare (256); one more is refused unread.
    while records < 256 {
        m.extend_from_slice(&[0xC0 | (name_at >> 8) as u8, name_at as u8]);
        srv(&mut m);
        records += 1;
    }
    m[6..8].copy_from_slice(&records.to_be_bytes());
    let before = violation_count();
    let parsed = assert_no_alloc(|| parse_announcements(&m).map(|a| a.len()));
    assert_eq!(parsed, Ok(0));
    assert_eq!(violation_count(), before, "parsing allocated");
    m[6..8].copy_from_slice(&(records + 1).to_be_bytes());
    let refused = assert_no_alloc(|| parse_announcements(&m).map(|a| a.len()));
    assert_eq!(
        refused,
        Err(jstream_jtunnel::dnssd::DnsError::TooManyRecords)
    );
    assert_eq!(violation_count(), before, "refusing allocated");
}
