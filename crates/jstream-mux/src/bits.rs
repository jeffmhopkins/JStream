//! A big-endian bit reader for the codec headers the muxers read: HEVC parameter sets
//! (after emulation prevention is removed) and the AV1 sequence header. Every read
//! returns `None` past the end, and the caller turns that into its own error.

/// Removes emulation-prevention bytes (`00 00 03` → `00 00`) from an H.264 or HEVC NAL
/// unit payload, giving the RBSP.
pub(crate) fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// Reads bits most-significant first.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// One bit.
    pub(crate) fn bit(&mut self) -> Option<u32> {
        let byte = self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(bit))
    }

    /// One bit, as a flag.
    pub(crate) fn flag(&mut self) -> Option<bool> {
        self.bit().map(|b| b == 1)
    }

    /// `n` bits (at most 32) as an unsigned integer.
    pub(crate) fn bits(&mut self, n: u32) -> Option<u32> {
        debug_assert!(n <= 32, "at most 32 bits at a time");
        let mut value = 0u64;
        for _ in 0..n {
            value = (value << 1) | u64::from(self.bit()?);
        }
        // At most 32 bits were read.
        Some(value as u32)
    }

    /// Skips `n` bits.
    pub(crate) fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len() * 8 {
            return None;
        }
        self.pos = end;
        Some(())
    }

    /// An unsigned Exp-Golomb code (`ue(v)`), at most 32 bits of value.
    pub(crate) fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = u64::from(self.bits(zeros)?);
        u32::try_from((1u64 << zeros) - 1 + rest).ok()
    }

    /// AV1's variable-length unsigned code (`uvlc()`), at most 32 bits of value.
    pub(crate) fn uvlc(&mut self) -> Option<u32> {
        // The same code as Exp-Golomb, except that 32 or more leading zeros mean 2^32 - 1.
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
        }
        if zeros >= 32 {
            return Some(u32::MAX);
        }
        let rest = u64::from(self.bits(zeros)?);
        u32::try_from((1u64 << zeros) - 1 + rest).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_and_codes() {
        // 101 | 0011 0 | 1 | 00101 ...
        let mut r = BitReader::new(&[0b1010_0110, 0b1001_0100]);
        assert_eq!(r.bits(3), Some(0b101));
        assert_eq!(r.ue(), Some(5), "00110 is ue 5");
        assert_eq!(r.flag(), Some(true));
        assert_eq!(r.ue(), Some(4), "00101 is ue 4");
        assert_eq!(r.skip(2), Some(()));
        assert_eq!(r.bit(), None);
        assert_eq!(r.skip(1), None);
    }

    #[test]
    fn exp_golomb_limits() {
        assert_eq!(BitReader::new(&[0; 5]).ue(), None);
        assert_eq!(BitReader::new(&[0, 0, 0, 0, 0x80]).uvlc(), Some(u32::MAX));
        assert_eq!(BitReader::new(&[0x40]).uvlc(), Some(1));
    }

    #[test]
    fn unescape_removes_emulation_prevention() {
        assert_eq!(
            unescape(&[0, 0, 3, 1, 0, 0, 3, 0, 0, 3]),
            [0, 0, 1, 0, 0, 0, 0]
        );
        assert_eq!(unescape(&[0, 3, 0, 0, 0, 3]), [0, 3, 0, 0, 0]);
    }
}
