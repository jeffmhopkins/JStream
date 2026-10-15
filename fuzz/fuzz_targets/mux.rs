//! The fMP4 and FLV muxers, fed arbitrary packet streams.
//!
//! The muxers parse each video payload (Annex B for H.264 and HEVC, OBUs for AV1) and
//! check timestamps, so a bad encoder packet or a clock jump must end in an error, never
//! a panic or an overflow. The input is read as a header (container, tracks, time base,
//! origin) and then packets (keyframe flag, track, PTS, DTS offset, payload).

#![no_main]

use jstream_media::{Codec, MediaTime, Packet, Timebase};
use jstream_mux::aac::AudioSpecificConfig;
use jstream_mux::flv::{self, ExVideoConfig, FlvConfig, FlvTagger, MetadataStyle};
use jstream_mux::fmp4::{self, Fmp4Config, Fmp4Muxer};
use libfuzzer_sys::fuzz_target;

/// Reads fixed-size values from the front of the input; `None` once it runs out.
struct Input<'a>(&'a [u8]);

impl<'a> Input<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
    }

    fn i64(&mut self) -> Option<i64> {
        self.take(8)
            .and_then(|b| b.try_into().ok())
            .map(i64::from_le_bytes)
    }
}

/// Feeds packets to one muxer until the input runs out, then finishes it.
trait Muxer {
    fn push(&mut self, packet: &Packet);
    fn finish(&mut self);
}

impl Muxer for Fmp4Muxer {
    fn push(&mut self, packet: &Packet) {
        let _ = Fmp4Muxer::push(self, packet);
    }

    fn finish(&mut self) {
        let _ = Fmp4Muxer::finish(self);
    }
}

impl Muxer for FlvTagger {
    fn push(&mut self, packet: &Packet) {
        let _ = FlvTagger::push(self, packet);
    }

    fn finish(&mut self) {
        let _ = FlvTagger::finish(self);
    }
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input(data);
    let Some(flags) = input.u8() else {
        return;
    };
    // A time base from the input: mostly the usual ones, sometimes anything.
    let timebase = match input.u8() {
        Some(0) => input
            .u32()
            .zip(input.u32())
            .and_then(|(num, den)| Timebase::new(num, den).ok()),
        Some(1) => Some(Timebase::NANOS),
        Some(2) => Some(Timebase::MPEG_90K),
        _ => Some(Timebase::MILLIS),
    };
    let Some(timebase) = timebase else {
        return;
    };
    let origin = if flags & 0x40 != 0 {
        let Some(ticks) = input.i64() else {
            return;
        };
        Some(MediaTime::new(ticks, timebase))
    } else {
        None
    };
    let has_audio = flags & 0x08 != 0;
    let audio = AudioSpecificConfig::aac_lc(48_000, 2).expect("a valid config");
    let video_codec = match (flags >> 1) & 0x03 {
        0 => None,
        1 => Some(Codec::H264),
        2 => Some(Codec::Hevc),
        _ => Some(Codec::Av1),
    };

    let mut muxer: Box<dyn Muxer> = if flags & 0x01 == 0 {
        // fMP4 carries H.264 only.
        let config = Fmp4Config {
            video: video_codec.map(|_| fmp4::VideoTrack {
                width: 1280,
                height: 720,
                avc: None,
            }),
            audio: has_audio.then_some(fmp4::AudioTrack {
                config: audio,
                bitrate_kbps: Some(128),
            }),
            origin,
        };
        let Ok(mut muxer) = Fmp4Muxer::new(config) else {
            return;
        };
        let _ = muxer.start();
        Box::new(muxer)
    } else {
        let config = FlvConfig {
            video: video_codec.map(|_| flv::VideoTrack {
                width: 1280,
                height: 720,
                frame_rate: Some(30.0),
                bitrate_kbps: Some(6000),
                avc: None,
            }),
            audio: has_audio.then_some(flv::AudioTrack {
                config: audio,
                bitrate_kbps: Some(128),
            }),
            origin,
            metadata_style: if flags & 0x10 != 0 {
                MetadataStyle::SetDataFrame
            } else {
                MetadataStyle::default()
            },
            encoder: None,
        };
        let tagger = match video_codec {
            Some(Codec::Hevc) => FlvTagger::with_enhanced_video(config, ExVideoConfig::Hevc(None)),
            Some(Codec::Av1) => FlvTagger::with_enhanced_video(config, ExVideoConfig::Av1(None)),
            _ => FlvTagger::new(config),
        };
        let Ok(mut tagger) = tagger else {
            return;
        };
        let _ = tagger.start();
        Box::new(tagger)
    };

    let video = video_codec.unwrap_or(Codec::H264);
    while let Some(head) = input.u8() {
        let (Some(pts), Some(back), Some(len)) = (input.i64(), input.u16(), input.u16()) else {
            break;
        };
        let Some(payload) = input.take(usize::from(len)) else {
            break;
        };
        let codec = if head & 0x02 != 0 { Codec::Aac } else { video };
        let dts = MediaTime::new(pts.saturating_sub(i64::from(back)), timebase);
        let pts = MediaTime::new(pts, timebase);
        if let Ok(packet) = Packet::new(codec, head & 0x01 != 0, pts, dts, payload.to_vec()) {
            muxer.push(&packet);
        }
    }
    muxer.finish();
});
