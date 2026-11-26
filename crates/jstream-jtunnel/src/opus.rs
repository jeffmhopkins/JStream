//! A stream's Opus frames, decoded once each and in order.
//!
//! JTunnel's default format is Opus at 48 kHz in 5 ms frames, and each packet carries the
//! frame before it again as a redundant copy. An Opus decoder keeps state from one frame
//! to the next, so [`OpusFrames`] decodes each frame once, in sequence order: a copy of a
//! frame it already decoded, or one older than that, is [`Decoded::Skipped`] (the caller
//! still hands it to the playout, which keeps its events, and which already holds the
//! decoded copy). A frame lost on the way is left out, and the decoder goes on from the
//! next one. A new stream ID, rate or frame length starts a fresh decoder.

use rusty_opus::OpusDecoder;

use crate::wire::{AudioHeader, CHANNELS, Codec, Frame};

/// What [`OpusFrames::decode`] did with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded {
    /// Decoded into this many interleaved samples at the front of the buffer.
    Samples(usize),
    /// A frame already decoded (a redundant copy), or one older than the last decoded:
    /// no samples.
    Skipped,
    /// The decoder refused the frame (a malformed Opus packet, or a rate, frame length or
    /// buffer it can't decode into): no samples.
    Refused,
}

/// One sender's Opus decoder and the last frame it decoded.
#[derive(Default)]
pub struct OpusFrames {
    decoder: Option<OpusDecoder>,
    /// The stream ID, rate and frame length the decoder was made for.
    stream: Option<(u32, u32, u16)>,
    /// The sequence number of the last frame decoded or refused.
    last: Option<u32>,
}

impl OpusFrames {
    /// No decoder yet: the first frame makes one.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes `frame` of an Opus packet with `header` into `out`, interleaved stereo at
    /// the header's rate, if it is the next frame to decode. `out` must hold the header's
    /// `frame_samples` times [`CHANNELS`] samples. The decoder never panics on a
    /// malformed packet; it allocates when a stream starts and while its first frames
    /// warm it up, on the receive thread, never on the audio thread.
    pub fn decode(&mut self, header: &AudioHeader, frame: &Frame<'_>, out: &mut [f32]) -> Decoded {
        if header.codec != Codec::Opus {
            return Decoded::Refused;
        }
        let stream = (header.stream_id, header.sample_rate, header.frame_samples);
        if self.stream != Some(stream) {
            self.stream = Some(stream);
            self.last = None;
            self.decoder = i32::try_from(header.sample_rate)
                .ok()
                .and_then(|rate| OpusDecoder::new(rate, usize::from(CHANNELS)).ok());
        }
        // A frame at or before the last one decoded, by wrapping sequence order.
        if let Some(last) = self.last
            && frame.seq.wrapping_sub(last) as i32 <= 0
        {
            return Decoded::Skipped;
        }
        self.last = Some(frame.seq);
        let Some(decoder) = self.decoder.as_mut() else {
            return Decoded::Refused;
        };
        let frame_samples = usize::from(header.frame_samples);
        let want = frame_samples * usize::from(CHANNELS);
        // An empty frame would ask the decoder to conceal one; a sender never sends one.
        if frame.audio.is_empty() || out.len() < want {
            return Decoded::Refused;
        }
        match decoder.decode(frame.audio, frame_samples, &mut out[..want]) {
            Ok(n) if n == frame_samples => Decoded::Samples(want),
            Ok(_) | Err(_) => Decoded::Refused,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{
        self, FrameOut, MAX_PACKET_BYTES, OPUS_FRAME_SAMPLES, OPUS_SAMPLE_RATE, Packet,
    };
    use rusty_opus::{Application, OpusEncoder};

    const N: usize = OPUS_FRAME_SAMPLES as usize;

    /// `count` Opus frames of a 1 kHz sine, as JTunnel codes them.
    fn coded(count: usize) -> Vec<Vec<u8>> {
        let mut encoder = OpusEncoder::new(48_000, 2, Application::Audio).expect("an encoder");
        let mut packets = Vec::new();
        for f in 0..count {
            let pcm: Vec<f32> = (0..N)
                .flat_map(|i| {
                    let t = (f * N + i) as f32 / 48_000.0;
                    let v = 0.5 * (std::f32::consts::TAU * 1_000.0 * t).sin();
                    [v, v]
                })
                .collect();
            let mut out = vec![0_u8; 1_000];
            let n = encoder.encode(&pcm, N, &mut out).expect("encoded");
            out.truncate(n);
            packets.push(out);
        }
        packets
    }

    fn header(stream_id: u32, seq: u32) -> AudioHeader {
        AudioHeader {
            stream_id,
            seq,
            sample_rate: OPUS_SAMPLE_RATE,
            codec: Codec::Opus,
            frame_samples: OPUS_FRAME_SAMPLES,
        }
    }

    /// Each packet as JTunnel sends it (the frame, then a copy of the one before), parsed
    /// and decoded oldest first, as the receiver does: what each frame gave.
    fn receive(frames: &mut OpusFrames, packets: &[Vec<u8>], seqs: &[u32]) -> Vec<(u32, Decoded)> {
        let mut got = Vec::new();
        let mut buf = [0_u8; MAX_PACKET_BYTES];
        let mut out = [0.0_f32; N * 2];
        for &seq in seqs {
            let i = seq as usize;
            let mut outs = vec![FrameOut {
                events: &[],
                audio: &packets[i],
            }];
            if i > 0 {
                outs.push(FrameOut {
                    events: &[],
                    audio: &packets[i - 1],
                });
            }
            let n = wire::write_audio(&mut buf, &header(7, seq), &outs).expect("a packet");
            let Ok(Packet::Audio {
                header,
                frames: parsed,
            }) = wire::parse(&buf[..n])
            else {
                panic!("an audio packet");
            };
            for frame in parsed.iter().rev().flatten() {
                got.push((frame.seq, frames.decode(&header, frame, &mut out)));
            }
        }
        got
    }

    #[test]
    fn each_frame_decodes_once_in_order_and_a_redundant_copy_is_skipped() {
        let packets = coded(6);
        let mut frames = OpusFrames::new();
        let got = receive(&mut frames, &packets, &[0, 1, 2]);
        let whole = Decoded::Samples(N * 2);
        assert_eq!(
            got,
            [
                (0, whole),
                (0, Decoded::Skipped),
                (1, whole),
                (1, Decoded::Skipped),
                (2, whole)
            ]
        );
        // Packet 4 is lost: its copy in packet 5 is decoded first, so nothing is missed.
        let got = receive(&mut frames, &packets, &[4, 5]);
        assert_eq!(
            got,
            [(3, whole), (4, whole), (4, Decoded::Skipped), (5, whole)]
        );
        // A packet reordered after newer ones is skipped whole.
        let got = receive(&mut frames, &packets, &[2]);
        assert_eq!(got, [(1, Decoded::Skipped), (2, Decoded::Skipped)]);
    }

    /// What `frames` gives for one single-frame packet with `header` and `audio`.
    fn one(
        frames: &mut OpusFrames,
        header: &AudioHeader,
        audio: &[u8],
        out: &mut [f32],
    ) -> Decoded {
        let mut buf = [0_u8; MAX_PACKET_BYTES];
        let n = wire::write_audio(&mut buf, header, &[FrameOut { events: &[], audio }])
            .expect("a packet");
        let Ok(Packet::Audio {
            header,
            frames: parsed,
        }) = wire::parse(&buf[..n])
        else {
            panic!("an audio packet");
        };
        let frame = parsed[0].expect("a frame");
        frames.decode(&header, &frame, out)
    }

    #[test]
    fn decoded_audio_carries_the_signal() {
        let packets = coded(20);
        let mut frames = OpusFrames::new();
        let mut out = [0.0_f32; N * 2];
        let mut peak = 0.0_f32;
        for (seq, packet) in (0_u32..).zip(&packets) {
            assert_eq!(
                one(&mut frames, &header(1, seq), packet, &mut out),
                Decoded::Samples(N * 2)
            );
            if seq > 4 {
                peak = out.iter().fold(peak, |p, v| p.max(v.abs()));
            }
        }
        assert!(
            (0.3..0.7).contains(&peak),
            "a half-scale sine comes back near half scale: {peak}"
        );
    }

    #[test]
    fn a_new_stream_starts_over_and_a_bad_packet_is_refused() {
        let packets = coded(3);
        let mut frames = OpusFrames::new();
        let mut out = [0.0_f32; N * 2];
        let whole = Decoded::Samples(N * 2);
        assert_eq!(
            one(&mut frames, &header(1, 10), &packets[0], &mut out),
            whole
        );
        // The sender restarted with a new stream ID and an older sequence number.
        assert_eq!(
            one(&mut frames, &header(2, 0), &packets[0], &mut out),
            whole
        );
        // A malformed packet, and a short buffer, are refused without a panic.
        assert_eq!(
            one(&mut frames, &header(2, 1), &[0xFF, 0x03], &mut out),
            Decoded::Refused
        );
        assert_eq!(
            one(&mut frames, &header(2, 2), &packets[2], &mut out[..10]),
            Decoded::Refused
        );
        // A header that isn't Opus's (the parser holds Opus to 48 kHz and 5 ms frames) is
        // never decoded as Opus.
        let mut buf = [0_u8; MAX_PACKET_BYTES];
        let n = wire::write_audio(
            &mut buf,
            &header(3, 0),
            &[FrameOut {
                events: &[],
                audio: &packets[0],
            }],
        )
        .expect("a packet");
        let Ok(Packet::Audio {
            header: mut pcm,
            frames: parsed,
        }) = wire::parse(&buf[..n])
        else {
            panic!("an audio packet");
        };
        pcm.codec = Codec::Pcm24;
        let frame = parsed[0].expect("a frame");
        assert_eq!(frames.decode(&pcm, &frame, &mut out), Decoded::Refused);
    }
}
