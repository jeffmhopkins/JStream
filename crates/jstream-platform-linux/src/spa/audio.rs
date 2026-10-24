//! SPA pods for the PipeWire audio streams (ADR-0060 §4): the one raw audio format a
//! stream asks for, and the format PipeWire settles on. Platform-independent, so it is
//! tested everywhere.
//!
//! A stream asks for interleaved `f32` at a fixed rate and channel layout, with the
//! channel positions, and PipeWire's stream adapter converts the node's own samples to
//! it. So the format is known before the stream connects, as
//! [`AudioIo::input_format`](jstream_platform::AudioIo::input_format) needs.

use jstream_platform::media::ChannelLayout;

use super::{Builder, PodReader, TYPE_ID, TYPE_INT, TYPE_OBJECT, first_value, value};
use super::{
    FORMAT_MEDIA_SUBTYPE, FORMAT_MEDIA_TYPE, MEDIA_SUBTYPE_RAW, PARAM_ENUM_FORMAT, Pod,
    TYPE_OBJECT_FORMAT,
};

/// `SPA_TYPE_Array`.
const TYPE_ARRAY: u32 = 13;
/// `SPA_MEDIA_TYPE_audio`.
const MEDIA_TYPE_AUDIO: u32 = 1;
// `enum spa_format` in <spa/param/format.h>.
const FORMAT_AUDIO_FORMAT: u32 = 0x10001;
const FORMAT_AUDIO_RATE: u32 = 0x10003;
const FORMAT_AUDIO_CHANNELS: u32 = 0x10004;
const FORMAT_AUDIO_POSITION: u32 = 0x10005;

/// `SPA_AUDIO_FORMAT_F32`: 32-bit float, interleaved, in native byte order
/// (`SPA_AUDIO_FORMAT_F32_LE` or `_BE`, <spa/param/audio/raw.h>).
pub(crate) const AUDIO_FORMAT_F32: u32 = if cfg!(target_endian = "little") {
    0x11b
} else {
    0x11c
};

// `enum spa_audio_channel` in <spa/param/audio/raw.h>.
const CHANNEL_MONO: u32 = 2;
const CHANNEL_FL: u32 = 3;
const CHANNEL_FR: u32 = 4;
const CHANNEL_FC: u32 = 5;
const CHANNEL_LFE: u32 = 6;
const CHANNEL_RL: u32 = 12;
const CHANNEL_RR: u32 = 13;

/// The SPA channel positions of `layout`, in the engine's order. 5.1 is front left,
/// front right, center, LFE, rear left, rear right: WAVEFORMATEXTENSIBLE's order,
/// whose back speakers are SPA's rear ones. `None` for a layout added later.
pub(crate) fn positions(layout: ChannelLayout) -> Option<&'static [u32]> {
    Some(match layout {
        ChannelLayout::Mono => &[CHANNEL_MONO],
        ChannelLayout::Stereo => &[CHANNEL_FL, CHANNEL_FR],
        ChannelLayout::Surround51 => &[
            CHANNEL_FL,
            CHANNEL_FR,
            CHANNEL_FC,
            CHANNEL_LFE,
            CHANNEL_RL,
            CHANNEL_RR,
        ],
        _ => return None,
    })
}

/// The engine's layout for a node with `channels` channels: 1, 2 and 6 map to mono,
/// stereo and 5.1, and anything else (or an unknown count) is taken as stereo, which
/// PipeWire's adapter mixes the node's channels down or up to.
pub(crate) fn layout_for(channels: Option<u32>) -> ChannelLayout {
    match channels {
        Some(1) => ChannelLayout::Mono,
        Some(6) => ChannelLayout::Surround51,
        _ => ChannelLayout::Stereo,
    }
}

/// The `EnumFormat` param an audio stream offers: raw interleaved `f32` at `rate`, in
/// `layout`, each value fixed rather than a choice. `None` for a layout with no SPA
/// positions here.
pub(crate) fn enum_format(rate: u32, layout: ChannelLayout) -> Option<Pod> {
    let pos = positions(layout)?;
    let channels = u32::try_from(pos.len()).unwrap_or(u32::MAX);
    Some(Builder::object(
        TYPE_OBJECT_FORMAT,
        PARAM_ENUM_FORMAT,
        &[
            (FORMAT_MEDIA_TYPE, value(TYPE_ID, &[MEDIA_TYPE_AUDIO])),
            (FORMAT_MEDIA_SUBTYPE, value(TYPE_ID, &[MEDIA_SUBTYPE_RAW])),
            (FORMAT_AUDIO_FORMAT, value(TYPE_ID, &[AUDIO_FORMAT_F32])),
            (FORMAT_AUDIO_RATE, value(TYPE_INT, &[rate])),
            (FORMAT_AUDIO_CHANNELS, value(TYPE_INT, &[channels])),
            (FORMAT_AUDIO_POSITION, array(TYPE_ID, pos)),
        ],
    ))
}

/// An array pod's bytes: a child size and type, then the values.
fn array(child_type: u32, values: &[u32]) -> Vec<u8> {
    let mut body = Builder::default();
    body.u32(4);
    body.u32(child_type);
    for v in values {
        body.u32(*v);
    }
    let mut b = Builder::default();
    b.pod(TYPE_ARRAY, &body.bytes);
    b.bytes
}

/// The raw audio format PipeWire settled on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AudioRaw {
    /// `enum spa_audio_format`.
    pub(crate) format: u32,
    pub(crate) rate: u32,
    pub(crate) channels: u32,
}

/// Parses the `Format` param PipeWire settled on. `None` when it is not raw audio with
/// a format, a non-zero rate and a channel count.
pub(crate) fn parse_format(pod: &[u8]) -> Option<AudioRaw> {
    let r = PodReader { data: pod };
    let size = usize::try_from(r.u32_at(0)?).ok()?;
    if r.u32_at(4)? != TYPE_OBJECT || 8usize.checked_add(size)? > pod.len() || size < 8 {
        return None;
    }
    if r.u32_at(8)? != TYPE_OBJECT_FORMAT {
        return None;
    }
    let end = 8 + size;
    let mut at = 16; // past the object type and id
    let (mut media_type, mut subtype, mut format, mut rate, mut channels) =
        (None, None, None, None, None);
    while at + 16 <= end {
        let key = r.u32_at(at)?;
        let value_at = at + 8;
        let value_size = usize::try_from(r.u32_at(value_at)?).ok()?;
        let (ty, words, len) = first_value(&r, value_at)?;
        match (key, ty, len) {
            (FORMAT_MEDIA_TYPE, TYPE_ID, 4..) => media_type = r.u32_at(words),
            (FORMAT_MEDIA_SUBTYPE, TYPE_ID, 4..) => subtype = r.u32_at(words),
            (FORMAT_AUDIO_FORMAT, TYPE_ID, 4..) => format = r.u32_at(words),
            (FORMAT_AUDIO_RATE, TYPE_INT, 4..) => rate = r.u32_at(words),
            (FORMAT_AUDIO_CHANNELS, TYPE_INT, 4..) => channels = r.u32_at(words),
            _ => {}
        }
        at = value_at + (8 + value_size).next_multiple_of(8);
    }
    if media_type != Some(MEDIA_TYPE_AUDIO) || subtype != Some(MEDIA_SUBTYPE_RAW) {
        return None;
    }
    Some(AudioRaw {
        format: format?,
        rate: rate.filter(|&r| r > 0)?,
        channels: channels.filter(|&c| c > 0)?,
    })
}

/// Whether the settled format is the one `enum_format(rate, layout)` asked for.
pub(crate) fn is_requested(settled: AudioRaw, rate: u32, layout: ChannelLayout) -> bool {
    settled.format == AUDIO_FORMAT_F32
        && settled.rate == rate
        && settled.channels as usize == layout.channel_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(pod: &[u8]) -> Vec<u32> {
        pod.chunks(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// The pod is the one `spa_pod_builder_add_object` writes for
    /// `SPA_AUDIO_INFO_RAW_INIT(.format = F32, .rate = 48000, .channels = 2, .position = { FL, FR })`.
    #[test]
    fn the_offered_audio_format_has_the_spa_layout() {
        let pod = enum_format(48_000, ChannelLayout::Stereo).unwrap();
        let w = words(&pod.bytes());
        // Object header: body size, type, then object type and param id.
        assert_eq!(w[1], TYPE_OBJECT);
        assert_eq!(w[0] as usize, pod.bytes().len() - 8);
        assert_eq!(&w[2..4], &[TYPE_OBJECT_FORMAT, PARAM_ENUM_FORMAT]);
        // Each property: key, flags, then a value pod padded to 8 bytes.
        assert_eq!(
            &w[4..10],
            &[FORMAT_MEDIA_TYPE, 0, 4, TYPE_ID, MEDIA_TYPE_AUDIO, 0]
        );
        assert_eq!(&w[10..16], &[FORMAT_MEDIA_SUBTYPE, 0, 4, TYPE_ID, 1, 0]);
        assert_eq!(
            &w[16..22],
            &[FORMAT_AUDIO_FORMAT, 0, 4, TYPE_ID, AUDIO_FORMAT_F32, 0]
        );
        assert_eq!(&w[22..28], &[FORMAT_AUDIO_RATE, 0, 4, TYPE_INT, 48_000, 0]);
        assert_eq!(&w[28..34], &[FORMAT_AUDIO_CHANNELS, 0, 4, TYPE_INT, 2, 0]);
        // The positions: an array of two ids, 16 bytes of body, already aligned.
        assert_eq!(
            &w[34..],
            &[
                FORMAT_AUDIO_POSITION,
                0,
                16,
                TYPE_ARRAY,
                4,
                TYPE_ID,
                CHANNEL_FL,
                CHANNEL_FR
            ]
        );
        if cfg!(target_endian = "little") {
            assert_eq!(AUDIO_FORMAT_F32, 283);
        }
    }

    #[test]
    fn layouts_have_their_positions_and_counts() {
        for layout in [
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            ChannelLayout::Surround51,
        ] {
            assert_eq!(positions(layout).unwrap().len(), layout.channel_count());
            let pod = enum_format(44_100, layout).unwrap();
            let parsed = parse_format(&pod.bytes()).unwrap();
            assert!(is_requested(parsed, 44_100, layout), "{layout:?}");
        }
        assert_eq!(positions(ChannelLayout::Surround51).unwrap()[4..], [12, 13]);
        assert_eq!(layout_for(Some(1)), ChannelLayout::Mono);
        assert_eq!(layout_for(Some(2)), ChannelLayout::Stereo);
        assert_eq!(layout_for(Some(6)), ChannelLayout::Surround51);
        assert_eq!(layout_for(Some(4)), ChannelLayout::Stereo);
        assert_eq!(layout_for(None), ChannelLayout::Stereo);
    }

    fn settled(format: u32, rate: u32, channels: u32, media_type: u32) -> Vec<u8> {
        Builder::object(
            TYPE_OBJECT_FORMAT,
            4,
            &[
                (FORMAT_MEDIA_TYPE, value(TYPE_ID, &[media_type])),
                (FORMAT_MEDIA_SUBTYPE, value(TYPE_ID, &[MEDIA_SUBTYPE_RAW])),
                (FORMAT_AUDIO_FORMAT, value(TYPE_ID, &[format])),
                (FORMAT_AUDIO_RATE, value(TYPE_INT, &[rate])),
                (FORMAT_AUDIO_CHANNELS, value(TYPE_INT, &[channels])),
            ],
        )
        .bytes()
    }

    #[test]
    fn the_settled_format_parses_and_is_checked() {
        let ok = parse_format(&settled(AUDIO_FORMAT_F32, 48_000, 2, MEDIA_TYPE_AUDIO)).unwrap();
        assert_eq!(
            ok,
            AudioRaw {
                format: AUDIO_FORMAT_F32,
                rate: 48_000,
                channels: 2
            }
        );
        assert!(is_requested(ok, 48_000, ChannelLayout::Stereo));
        assert!(!is_requested(ok, 44_100, ChannelLayout::Stereo));
        assert!(!is_requested(ok, 48_000, ChannelLayout::Mono));
        // S16_LE isn't what was asked for.
        let s16 = parse_format(&settled(0x103, 48_000, 2, MEDIA_TYPE_AUDIO)).unwrap();
        assert!(!is_requested(s16, 48_000, ChannelLayout::Stereo));
        // Video, a zero rate or no channels isn't audio JStream can take.
        assert_eq!(parse_format(&settled(AUDIO_FORMAT_F32, 48_000, 2, 2)), None);
        assert_eq!(
            parse_format(&settled(AUDIO_FORMAT_F32, 0, 2, MEDIA_TYPE_AUDIO)),
            None
        );
        assert_eq!(
            parse_format(&settled(AUDIO_FORMAT_F32, 48_000, 0, MEDIA_TYPE_AUDIO)),
            None
        );
    }

    #[test]
    fn truncated_formats_are_refused() {
        let pod = settled(AUDIO_FORMAT_F32, 48_000, 2, MEDIA_TYPE_AUDIO);
        for len in 0..pod.len() {
            assert_eq!(parse_format(&pod[..len]), None, "cut at {len}");
        }
    }
}
