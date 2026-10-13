//! The AAC `AudioSpecificConfig` (ISO/IEC 14496-3), which FLV and MP4 carry once ahead of
//! the audio frames.
//!
//! JStream carries AAC [`Packet`](jstream_media::Packet) payloads as **raw access units**
//! without ADTS headers (Media Foundation's `MF_MT_AAC_PAYLOAD_TYPE = 0`), which is what
//! FLV and MP4 store.

use crate::MuxError;

/// The sampling frequencies with a 4-bit index; other rates use the explicit escape.
const FREQUENCIES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// Audio object type of AAC Low Complexity, the profile YouTube and most players expect.
pub const AOT_AAC_LC: u8 = 2;

/// An AAC `AudioSpecificConfig`: the serialized bytes plus the fields read from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSpecificConfig {
    bytes: Vec<u8>,
    object_type: u8,
    sample_rate: u32,
    channels: u8,
}

impl AudioSpecificConfig {
    /// The two-byte (or five-byte, for a rate without an index) config of a plain AAC
    /// stream: `object_type`, `sample_rate`, channel configuration `channels` (1 to 7), and
    /// a `GASpecificConfig` of 1024-sample frames with no core coder or extension.
    ///
    /// ```
    /// use jstream_mux::aac::{AOT_AAC_LC, AudioSpecificConfig};
    ///
    /// let asc = AudioSpecificConfig::new(AOT_AAC_LC, 48_000, 2)?;
    /// assert_eq!(asc.as_bytes(), [0x11, 0x90]);
    /// # Ok::<(), jstream_mux::MuxError>(())
    /// ```
    ///
    /// # Errors
    /// [`MuxError::InvalidAudioConfig`] when `object_type` is 0 or 31 and above, the
    /// sample rate is 0 or needs more than 24 bits, or `channels` is not 1 to 7.
    pub fn new(object_type: u8, sample_rate: u32, channels: u8) -> Result<Self, MuxError> {
        if object_type == 0 || object_type >= 31 {
            return Err(MuxError::InvalidAudioConfig {
                reason: "object type must be 1 to 30",
            });
        }
        if sample_rate == 0 || sample_rate >= 1 << 24 {
            return Err(MuxError::InvalidAudioConfig {
                reason: "sample rate must be 1 to 2^24 - 1",
            });
        }
        if !(1..=7).contains(&channels) {
            return Err(MuxError::InvalidAudioConfig {
                reason: "channel configuration must be 1 to 7",
            });
        }
        let mut bits = BitWriter::default();
        bits.put(u32::from(object_type), 5);
        match FREQUENCIES.iter().position(|&f| f == sample_rate) {
            // The table has 13 entries, so the index fits 4 bits.
            Some(index) => bits.put(index as u32, 4),
            None => {
                bits.put(0xF, 4);
                bits.put(sample_rate, 24);
            }
        }
        bits.put(u32::from(channels), 4);
        bits.put(0, 3); // frameLengthFlag, dependsOnCoreCoder, extensionFlag
        Ok(Self {
            bytes: bits.finish(),
            object_type,
            sample_rate,
            channels,
        })
    }

    /// An AAC-LC config ([`AOT_AAC_LC`]).
    ///
    /// # Errors
    /// As [`AudioSpecificConfig::new`].
    pub fn aac_lc(sample_rate: u32, channels: u8) -> Result<Self, MuxError> {
        Self::new(AOT_AAC_LC, sample_rate, channels)
    }

    /// A config an encoder produced (for Media Foundation, the tail of `MF_MT_USER_DATA`).
    /// The bytes are kept as given; the object type, rate and channels are read from the
    /// front.
    ///
    /// # Errors
    /// [`MuxError::InvalidAudioConfig`] when the bytes are too short, use the escaped
    /// object type (31), or carry a reserved frequency index.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, MuxError> {
        let truncated = MuxError::InvalidAudioConfig {
            reason: "too short",
        };
        let mut bits = BitReader {
            data: &bytes,
            pos: 0,
        };
        let object_type = bits.get(5).ok_or(truncated)?;
        if object_type == 0 || object_type == 31 {
            return Err(MuxError::InvalidAudioConfig {
                reason: "object type 0 or escaped object types are not supported",
            });
        }
        let index = bits.get(4).ok_or(MuxError::InvalidAudioConfig {
            reason: "too short",
        })?;
        let sample_rate = match index {
            0xF => bits.get(24).ok_or(MuxError::InvalidAudioConfig {
                reason: "too short",
            })?,
            i => *FREQUENCIES
                .get(i as usize)
                .ok_or(MuxError::InvalidAudioConfig {
                    reason: "reserved sampling frequency index",
                })?,
        };
        let channels = bits.get(4).ok_or(MuxError::InvalidAudioConfig {
            reason: "too short",
        })?;
        // `get(5)` and `get(4)` return at most 31 and 15.
        Ok(Self {
            object_type: object_type as u8,
            sample_rate,
            channels: channels as u8,
            bytes,
        })
    }

    /// The serialized config.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The audio object type (2 for AAC-LC).
    pub fn object_type(&self) -> u8 {
        self.object_type
    }

    /// The sampling frequency, in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The channel configuration (2 for stereo; 0 means defined in-band).
    pub fn channels(&self) -> u8 {
        self.channels
    }
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bits: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            let bit = ((value >> i) & 1) as u8;
            if let Some(last) = self.bytes.last_mut() {
                *last |= bit << (7 - self.bits % 8);
            }
            self.bits += 1;
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn get(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            let byte = self.data.get(self.pos / 8)?;
            v = (v << 1) | u32::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::{AOT_AAC_LC, AudioSpecificConfig};
    use crate::MuxError;

    #[test]
    fn common_configs_are_byte_exact() {
        // 00010 0011 0010 000 -> 0x11 0x90
        assert_eq!(
            AudioSpecificConfig::aac_lc(48_000, 2).unwrap().as_bytes(),
            [0x11, 0x90]
        );
        // 00010 0100 0010 000 -> 0x12 0x10
        assert_eq!(
            AudioSpecificConfig::aac_lc(44_100, 2).unwrap().as_bytes(),
            [0x12, 0x10]
        );
        // 00010 0100 0001 000 -> 0x12 0x08
        assert_eq!(
            AudioSpecificConfig::aac_lc(44_100, 1).unwrap().as_bytes(),
            [0x12, 0x08]
        );
    }

    #[test]
    fn explicit_frequency_uses_the_escape() {
        // 00010 1111 [24 bits of 50000 = 0x00C350] 0010 000 -> 39 bits in 5 bytes
        let asc = AudioSpecificConfig::aac_lc(50_000, 2).unwrap();
        assert_eq!(asc.as_bytes(), [0x17, 0x80, 0x61, 0xA8, 0x10]);
        let back = AudioSpecificConfig::from_bytes(asc.as_bytes().to_vec()).unwrap();
        assert_eq!(back, asc);
    }

    #[test]
    fn from_bytes_reads_the_fields() {
        // Media Foundation's AAC-LC 48 kHz stereo config
        let asc = AudioSpecificConfig::from_bytes(vec![0x11, 0x90]).unwrap();
        assert_eq!(
            (asc.object_type(), asc.sample_rate(), asc.channels()),
            (AOT_AAC_LC, 48_000, 2)
        );
        // extra bytes (e.g. SBR signaling) are kept as given
        let asc = AudioSpecificConfig::from_bytes(vec![0x12, 0x10, 0x56, 0xE5, 0x00]).unwrap();
        assert_eq!(asc.as_bytes().len(), 5);
        assert_eq!(asc.sample_rate(), 44_100);
    }

    #[test]
    fn invalid_configs_are_rejected() {
        let invalid = |r: Result<AudioSpecificConfig, MuxError>| {
            matches!(r, Err(MuxError::InvalidAudioConfig { .. }))
        };
        assert!(invalid(AudioSpecificConfig::new(0, 48_000, 2)));
        assert!(invalid(AudioSpecificConfig::new(31, 48_000, 2)));
        assert!(invalid(AudioSpecificConfig::aac_lc(0, 2)));
        assert!(invalid(AudioSpecificConfig::aac_lc(1 << 24, 2)));
        assert!(invalid(AudioSpecificConfig::aac_lc(48_000, 0)));
        assert!(invalid(AudioSpecificConfig::aac_lc(48_000, 8)));
        assert!(invalid(AudioSpecificConfig::from_bytes(vec![0x11])));
        assert!(invalid(AudioSpecificConfig::from_bytes(vec![])));
        assert!(invalid(AudioSpecificConfig::from_bytes(vec![0xF8, 0x00])));
        // frequency index 13 is reserved: 00010 1101 ...
        assert!(invalid(AudioSpecificConfig::from_bytes(vec![0x16, 0x90])));
    }
}
