use symphonia::core::audio::Position;

use super::{Head, OpusStream};

/// An `OpusHead` of `channels`, with `family` and, for a family other than 0, its
/// stream counts and mapping.
fn head(channels: u8, family: u8, gain_q8: i16, table: &[u8]) -> Vec<u8> {
    let mut out = b"OpusHead".to_vec();
    out.push(1);
    out.push(channels);
    out.extend_from_slice(&312_u16.to_le_bytes());
    out.extend_from_slice(&48_000_u32.to_le_bytes());
    out.extend_from_slice(&gain_q8.to_le_bytes());
    out.push(family);
    out.extend_from_slice(table);
    out
}

#[test]
fn reads_a_stereo_header() {
    let h = Head::parse(&head(2, 0, 0, &[])).expect("parses");
    assert_eq!((h.channels, h.pre_skip), (2, 312));
    assert_eq!(
        (h.streams, h.coupled, h.mapping.as_slice()),
        (1, 1, &[0, 1][..])
    );
    assert_eq!(
        h.positions(),
        Some(vec![Position::FRONT_LEFT, Position::FRONT_RIGHT])
    );
    assert!((h.gain - 1.0).abs() < 1e-6);
}

#[test]
fn reads_a_five_one_header_in_vorbis_order() {
    let h = Head::parse(&head(6, 1, 0, &[4, 2, 0, 4, 1, 2, 3, 5])).expect("parses");
    assert_eq!((h.streams, h.coupled), (4, 2));
    let positions = h.positions().expect("positions");
    assert_eq!(positions[1], Position::FRONT_CENTER);
    assert_eq!(positions[5], Position::LFE1);
    assert!(OpusStream::new(&h).is_ok());
}

#[test]
fn applies_the_output_gain() {
    // +6 dB in Q7.8.
    let h = Head::parse(&head(1, 0, 6 * 256, &[])).expect("parses");
    assert!((h.gain - 1.995).abs() < 0.01, "{}", h.gain);
}

#[test]
fn refuses_headers_it_cant_decode() {
    assert_eq!(Head::parse(b"OpusTags"), None);
    // Version 2.x.
    let mut v2 = head(2, 0, 0, &[]);
    v2[8] = 0x20;
    assert_eq!(Head::parse(&v2), None);
    // No channels, or three on family 0.
    assert_eq!(Head::parse(&head(0, 0, 0, &[])), None);
    assert_eq!(Head::parse(&head(3, 0, 0, &[])), None);
    // A mapping past the decoded channels, more coupled than streams, or cut short.
    assert_eq!(Head::parse(&head(2, 1, 0, &[1, 0, 0, 1])), None);
    assert_eq!(Head::parse(&head(2, 1, 0, &[1, 2, 0, 1])), None);
    assert_eq!(Head::parse(&head(2, 1, 0, &[1, 1, 0])), None);
    // 255 is silence, not out of range.
    assert!(Head::parse(&head(2, 1, 0, &[1, 0, 0, 255])).is_some());
}

#[test]
fn a_damaged_packet_is_an_error_not_a_panic() {
    let h = Head::parse(&head(2, 0, 0, &[])).expect("parses");
    let mut stream = OpusStream::new(&h).expect("decoder");
    for packet in [&[0xFF_u8, 0xFF, 0xFF][..], &[0x03], &[0x0B, 0xFF]] {
        let _ = stream.decode(packet);
    }
}
