//! An MP3's gapless information, read from the file itself (#445): the Xing or Info
//! header in its first frame, with the LAME tag after it, which say how many frames the
//! file holds and how much priming and padding the encoder added.
//!
//! The #445 rerun found a 56 ms hole at each wrap of a looping MP3, which is what is left
//! when nothing is cut: the Source Reader reported no `MF_SD_AUDIO_ENCODER_DELAY` for it,
//! and the hole is larger than priming and padding alone can make, so the decoder most
//! likely decodes the Info frame too, whose audio is silence. The reader cuts as if it
//! does, and the first whole pass's frame count corrects that for the passes after it.

/// What a decoder adds before the encoder's priming: an MP3 decoder's output lags its
/// input by 529 frames (528 plus one), which the LAME tag's delay doesn't count.
const DECODER_DELAY: u32 = 529;

/// An MP3's gapless numbers, from its Xing or Info header and LAME tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Mp3Gapless {
    /// Frames per MPEG frame: 1152 for MPEG-1 Layer III, 576 for MPEG-2 and 2.5.
    pub(super) frame_samples: u32,
    /// The MPEG frames of audio after the header frame, when the header says.
    pub(super) frames: Option<u32>,
    /// The encoder's priming, in frames.
    pub(super) encoder_delay: u32,
    /// The encoder's padding at the end, in frames.
    pub(super) encoder_padding: u32,
}

impl Mp3Gapless {
    /// The frames to drop at the start of the decoded stream: the encoder's priming and
    /// the decoder's delay, plus the header frame when the decoder decoded it.
    pub(super) fn delay(&self, header_decoded: bool) -> u32 {
        let header = if header_decoded {
            self.frame_samples
        } else {
            0
        };
        header + self.encoder_delay + DECODER_DELAY
    }

    /// The frames to drop at the end: the encoder's padding, less the decoder's delay
    /// that the start already accounts for.
    pub(super) fn padding(&self) -> u32 {
        self.encoder_padding.saturating_sub(DECODER_DELAY)
    }

    /// Whether `decoded` frames in one pass mean the decoder decoded the header frame
    /// too: closer to one frame more than the header's count than to the count itself.
    /// `None` when the header has no count.
    pub(super) fn header_was_decoded(&self, decoded: u64) -> Option<bool> {
        let frames = u64::from(self.frames?) * u64::from(self.frame_samples);
        let with_header = frames + u64::from(self.frame_samples);
        Some(decoded.abs_diff(with_header) < decoded.abs_diff(frames))
    }
}

/// The gapless numbers in the MP3 file whose first bytes are `head` (the first few
/// kilobytes are enough), or `None` when its first frame has no Xing or Info header
/// followed by a LAME tag.
pub(super) fn read(head: &[u8]) -> Option<Mp3Gapless> {
    let start = first_frame(head, id3_size(head))?;
    let header = head.get(start..start + 4)?;
    // MPEG version: 3 is MPEG-1, 2 MPEG-2, 0 MPEG-2.5. Layer bits 1 are Layer III.
    let version = (header[1] >> 3) & 3;
    if version == 1 || (header[1] >> 1) & 3 != 1 {
        return None;
    }
    let mpeg1 = version == 3;
    let crc = header[1] & 1 == 0;
    let mono = header[3] >> 6 == 3;
    let side_info = match (mpeg1, mono) {
        (true, true) => 17,
        (true, false) => 32,
        (false, true) => 9,
        (false, false) => 17,
    };
    let mut at = start + 4 + if crc { 2 } else { 0 } + side_info;
    let tag = head.get(at..at + 4)?;
    if tag != b"Xing" && tag != b"Info" {
        return None;
    }
    let flags = be32(head, at + 4)?;
    at += 8;
    let mut frames = None;
    if flags & 1 != 0 {
        frames = Some(be32(head, at)?);
        at += 4;
    }
    if flags & 2 != 0 {
        at += 4; // bytes
    }
    if flags & 4 != 0 {
        at += 100; // table of contents
    }
    if flags & 8 != 0 {
        at += 4; // quality
    }
    // The LAME tag: a 9-byte encoder name, then at 21 the delay and padding, 12 bits
    // each. LAME writes it, and so does FFmpeg's encoder (as "Lavc" or "Lavf").
    let name = head.get(at..at + 4)?;
    if name != b"LAME" && name != b"Lavc" && name != b"Lavf" {
        return None;
    }
    let delays = head.get(at + 21..at + 24)?;
    let encoder_delay = (u32::from(delays[0]) << 4) | (u32::from(delays[1]) >> 4);
    let encoder_padding = (u32::from(delays[1] & 0x0F) << 8) | u32::from(delays[2]);
    Some(Mp3Gapless {
        frame_samples: if mpeg1 { 1152 } else { 576 },
        frames,
        encoder_delay,
        encoder_padding,
    })
}

/// The bytes of an ID3v2 tag at the start of `head`, its footer included, or 0.
fn id3_size(head: &[u8]) -> usize {
    let Some(tag) = head.get(..10).filter(|tag| tag.starts_with(b"ID3")) else {
        return 0;
    };
    let size = tag[6..10]
        .iter()
        .fold(0usize, |size, byte| (size << 7) | usize::from(byte & 0x7F));
    let footer = if tag[5] & 0x10 != 0 { 10 } else { 0 };
    10 + size + footer
}

/// Where the first MPEG frame header at or after `from` starts: eleven set sync bits.
fn first_frame(head: &[u8], from: usize) -> Option<usize> {
    let rest = head.get(from..)?;
    rest.windows(2)
        .position(|pair| pair[0] == 0xFF && pair[1] & 0xE0 == 0xE0)
        .map(|at| from + at)
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::{Mp3Gapless, read};

    /// How [`mp3`] lays out the first frame.
    #[derive(Clone, Copy)]
    struct Layout {
        /// The second header byte: sync bits, MPEG version, layer and CRC bit.
        header1: u8,
        /// Channel mode 3 (mono) instead of joint stereo.
        mono: bool,
        /// Side info bytes after the header (and CRC).
        side_info: usize,
        /// The Xing flags: 1 frames, 2 bytes, 4 table of contents, 8 quality.
        flags: u32,
        /// An ID3v2.4 tag with a footer, instead of one without.
        footer: bool,
        encoder: &'static [u8; 4],
    }

    /// MPEG-1 Layer III joint stereo without CRC, with only a frame count.
    const MPEG1: Layout = Layout {
        header1: 0xFB,
        mono: false,
        side_info: 32,
        flags: 1,
        footer: false,
        encoder: b"LAME",
    };

    /// The first bytes of an MP3: an optional ID3v2 tag of `id3` bytes holding the start
    /// of a JPEG, then a Layer III frame header laid out as `layout` says, its side info,
    /// and an Info header with a frame count and a LAME tag of `delay` and `padding`.
    fn mp3_with(layout: Layout, id3: usize, frames: u32, delay: u32, padding: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        if id3 > 0 {
            let size = id3 - 10;
            let flags = if layout.footer { 0x10 } else { 0 };
            bytes.extend([b'I', b'D', b'3', 4, 0, flags]);
            for shift in [21, 14, 7, 0] {
                bytes.push(u8::try_from((size >> shift) & 0x7F).unwrap_or(0));
            }
            // Cover art: a JPEG's start and APP0 markers look like a frame's sync bits.
            bytes.extend([0xFF, 0xD8, 0xFF, 0xE0]);
            bytes.resize(id3, 0);
            if layout.footer {
                bytes.extend(b"3DI\x04\x00\x10");
                bytes.resize(bytes.len() + 4, 0);
            }
        }
        let mode = if layout.mono { 0xC4 } else { 0x44 };
        bytes.extend([0xFF, layout.header1, 0x90, mode]);
        bytes.resize(bytes.len() + layout.side_info, 0);
        bytes.extend(b"Info");
        bytes.extend(layout.flags.to_be_bytes());
        if layout.flags & 1 != 0 {
            bytes.extend(frames.to_be_bytes());
        }
        if layout.flags & 2 != 0 {
            bytes.extend(417_792u32.to_be_bytes());
        }
        if layout.flags & 4 != 0 {
            bytes.extend((0..100u8).map(|at| at.wrapping_mul(2)));
        }
        if layout.flags & 8 != 0 {
            bytes.extend(57u32.to_be_bytes());
        }
        bytes.extend(layout.encoder);
        bytes.extend(b"3.100");
        bytes.resize(bytes.len() + 12, 0);
        bytes.push(u8::try_from(delay >> 4).unwrap_or(0));
        bytes.push(u8::try_from(((delay & 0x0F) << 4) | (padding >> 8)).unwrap_or(0));
        bytes.push(u8::try_from(padding & 0xFF).unwrap_or(0));
        bytes.resize(bytes.len() + 64, 0);
        bytes
    }

    fn mp3(id3: usize, frames: u32, delay: u32, padding: u32) -> Vec<u8> {
        mp3_with(MPEG1, id3, frames, delay, padding)
    }

    #[test]
    fn the_lame_tag_gives_the_priming_padding_and_frame_count() {
        for id3 in [0, 1034] {
            let gapless = read(&mp3(id3, 192, 576, 1260));
            assert_eq!(
                gapless,
                Some(Mp3Gapless {
                    frame_samples: 1152,
                    frames: Some(192),
                    encoder_delay: 576,
                    encoder_padding: 1260,
                }),
                "ID3 tag of {id3} bytes"
            );
        }
    }

    #[test]
    fn the_cut_counts_the_decoder_and_the_header_frame() {
        let gapless = read(&mp3(0, 192, 576, 1260)).expect("a LAME tag");
        assert_eq!(gapless.delay(true), 1152 + 576 + 529);
        assert_eq!(gapless.delay(false), 576 + 529);
        assert_eq!(gapless.padding(), 1260 - 529);
        let small = Mp3Gapless {
            encoder_padding: 100,
            ..gapless
        };
        assert_eq!(small.padding(), 0, "never negative");
    }

    #[test]
    fn a_pass_shows_whether_the_header_frame_was_decoded() {
        let gapless = read(&mp3(0, 192, 576, 1260)).expect("a LAME tag");
        assert_eq!(gapless.header_was_decoded(193 * 1152), Some(true));
        assert_eq!(gapless.header_was_decoded(192 * 1152), Some(false));
        assert_eq!(gapless.header_was_decoded(192 * 1152 + 100), Some(false));
        assert_eq!(
            gapless.header_was_decoded(192 * 1152 + 576),
            Some(false),
            "halfway counts as not decoded"
        );
        let uncounted = Mp3Gapless {
            frames: None,
            ..gapless
        };
        assert_eq!(uncounted.header_was_decoded(193 * 1152), None);
    }

    #[test]
    fn every_layout_lame_writes_puts_the_tag_where_it_is_read() {
        let full = Layout {
            flags: 0x0F,
            ..MPEG1
        };
        let cases = [
            ("all four Xing fields, as LAME writes them", full, 1152),
            (
                "MPEG-1 mono",
                Layout {
                    mono: true,
                    side_info: 17,
                    ..full
                },
                1152,
            ),
            (
                "MPEG-1 with CRC",
                Layout {
                    header1: 0xFA,
                    side_info: 34,
                    ..full
                },
                1152,
            ),
            (
                "MPEG-2 stereo",
                Layout {
                    header1: 0xF3,
                    side_info: 17,
                    ..full
                },
                576,
            ),
            (
                "MPEG-2 mono",
                Layout {
                    header1: 0xF3,
                    mono: true,
                    side_info: 9,
                    ..full
                },
                576,
            ),
            (
                "MPEG-2.5 stereo",
                Layout {
                    header1: 0xE3,
                    side_info: 17,
                    ..full
                },
                576,
            ),
            (
                "FFmpeg's encoder",
                Layout {
                    encoder: b"Lavc",
                    ..full
                },
                1152,
            ),
            (
                "FFmpeg's muxer",
                Layout {
                    encoder: b"Lavf",
                    ..full
                },
                1152,
            ),
        ];
        for (what, layout, frame_samples) in cases {
            for id3 in [0, 1034] {
                assert_eq!(
                    read(&mp3_with(layout, id3, 192, 576, 1260)),
                    Some(Mp3Gapless {
                        frame_samples,
                        frames: Some(192),
                        encoder_delay: 576,
                        encoder_padding: 1260,
                    }),
                    "{what}, ID3 tag of {id3} bytes"
                );
            }
        }
        let footer = Layout {
            footer: true,
            ..full
        };
        assert_eq!(
            read(&mp3_with(footer, 1034, 192, 576, 1260)).map(|gapless| gapless.encoder_delay),
            Some(576),
            "an ID3v2.4 tag with a footer"
        );
        let uncounted = Layout {
            flags: 0x0E,
            ..full
        };
        assert_eq!(
            read(&mp3_with(uncounted, 0, 192, 576, 1260)).map(|gapless| gapless.frames),
            Some(None),
            "no frame count"
        );
    }

    #[test]
    fn headers_that_are_not_layer_three_give_nothing() {
        // Laid out as MPEG-2 would be, so only the version bits are wrong.
        let reserved = Layout {
            header1: 0xEB,
            side_info: 17,
            ..MPEG1
        };
        assert_eq!(
            read(&mp3_with(reserved, 0, 192, 576, 1260)),
            None,
            "MPEG version 01"
        );
        let layer2 = Layout {
            header1: 0xFD,
            ..MPEG1
        };
        assert_eq!(read(&mp3_with(layer2, 0, 192, 576, 1260)), None, "Layer II");
    }

    #[test]
    fn files_without_a_lame_tag_give_nothing() {
        assert_eq!(read(b""), None);
        assert_eq!(read(b"RIFF....WAVEfmt "), None);
        let mut no_tag = mp3(0, 192, 576, 1260);
        no_tag[36..40].copy_from_slice(b"Abcd");
        assert_eq!(read(&no_tag), None, "no Xing or Info header");
        let mut other = mp3(0, 192, 576, 1260);
        other[48..52].copy_from_slice(b"Abcd");
        assert_eq!(read(&other), None, "an encoder that writes no LAME tag");
        assert_eq!(read(&mp3(0, 192, 576, 1260)[..60]), None, "cut short");
    }
}
