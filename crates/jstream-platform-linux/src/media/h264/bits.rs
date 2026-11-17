//! Reading an H.264 NAL unit's RBSP bit by bit (ITU-T H.264 §7.2): fixed-width fields,
//! Exp-Golomb codes, and the emulation-prevention bytes removed first.

/// Why a NAL unit didn't parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// The unit ended inside a field.
    Truncated,
    /// A field holds a value the standard forbids or this parser doesn't take.
    Invalid(&'static str),
    /// A valid stream that uses a tool the decoder doesn't support.
    Unsupported(&'static str),
}

pub(crate) type Result<T> = std::result::Result<T, ParseError>;

/// `nal` with its emulation-prevention bytes removed: each `00 00 03` becomes `00 00`.
pub(crate) fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in nal {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// A big-endian bit reader over an RBSP.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// The next bit, counted from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// The bits read so far.
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// `u(1)`.
    pub(crate) fn flag(&mut self) -> Result<bool> {
        let byte = *self.data.get(self.pos / 8).ok_or(ParseError::Truncated)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(bit == 1)
    }

    /// `u(n)` for `n` up to 32.
    pub(crate) fn bits(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        let mut value: u32 = 0;
        for _ in 0..n {
            value = (value << 1) | u32::from(self.flag()?);
        }
        Ok(value)
    }

    /// Skips `n` bits.
    pub(crate) fn skip(&mut self, n: usize) -> Result<()> {
        if self.pos + n > self.data.len() * 8 {
            return Err(ParseError::Truncated);
        }
        self.pos += n;
        Ok(())
    }

    /// `ue(v)`, up to `2^32 - 2`.
    pub(crate) fn ue(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            if zeros > 31 {
                return Err(ParseError::Invalid(
                    "an Exp-Golomb code longer than 32 bits",
                ));
            }
        }
        if zeros == 0 {
            return Ok(0);
        }
        let rest = self.bits(zeros)?;
        ((1_u64 << zeros) - 1 + u64::from(rest))
            .try_into()
            .map_err(|_| ParseError::Invalid("an Exp-Golomb code out of range"))
    }

    /// `ue(v)` no larger than `max`.
    pub(crate) fn ue_max(&mut self, max: u32, what: &'static str) -> Result<u32> {
        let value = self.ue()?;
        if value > max {
            return Err(ParseError::Invalid(what));
        }
        Ok(value)
    }

    /// `se(v)`.
    pub(crate) fn se(&mut self) -> Result<i32> {
        let code = i64::from(self.ue()?);
        let value = if code % 2 == 1 {
            (code + 1) / 2
        } else {
            -(code / 2)
        };
        i32::try_from(value)
            .map_err(|_| ParseError::Invalid("a signed Exp-Golomb code out of range"))
    }

    /// `se(v)` within `min..=max`.
    pub(crate) fn se_range(&mut self, min: i32, max: i32, what: &'static str) -> Result<i32> {
        let value = self.se()?;
        if !(min..=max).contains(&value) {
            return Err(ParseError::Invalid(what));
        }
        Ok(value)
    }

    /// `more_rbsp_data()`: whether anything but the RBSP trailing bits is left.
    pub(crate) fn more_rbsp_data(&self) -> bool {
        // The last 1 bit in the data is the stop bit; trailing zero bytes (cabac_zero_words)
        // come after it.
        let Some(last) = self.data.iter().rposition(|&b| b != 0) else {
            return false;
        };
        let stop = last * 8 + 7 - self.data[last].trailing_zeros() as usize;
        self.pos < stop
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReader, ParseError, unescape};

    #[test]
    fn removes_emulation_prevention_bytes() {
        assert_eq!(
            unescape(&[0x00, 0x00, 0x03, 0x01, 0x00, 0x00, 0x03, 0x00, 0x03]),
            vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x03]
        );
        assert_eq!(unescape(&[0x00, 0x03, 0x00]), vec![0x00, 0x03, 0x00]);
    }

    #[test]
    fn reads_exp_golomb_codes() {
        // 1 | 010 | 011 | 00100 | 00101 | 0001000 -> 0, 1, 2, 3, 4, 7
        let data = [0b1010_0110, 0b0100_0010, 0b1000_1000];
        let mut r = BitReader::new(&data);
        assert_eq!(r.ue(), Ok(0));
        assert_eq!(r.ue(), Ok(1));
        assert_eq!(r.ue(), Ok(2));
        assert_eq!(r.ue(), Ok(3));
        assert_eq!(r.ue(), Ok(4));
        assert_eq!(r.ue(), Ok(7));
        assert_eq!(r.position(), 24);
        assert_eq!(r.flag(), Err(ParseError::Truncated));
        // se: codes 1, 2, 3, 4 -> 1, -1, 2, -2
        let data = [0b0100_1100, 0b1000_0101];
        let mut r = BitReader::new(&data);
        assert_eq!(r.se(), Ok(1));
        assert_eq!(r.se(), Ok(-1));
        assert_eq!(r.se(), Ok(2));
        assert_eq!(r.se(), Ok(-2));
    }

    #[test]
    fn finds_the_trailing_bits() {
        let data = [0b1100_0000, 0x80, 0x00];
        let mut r = BitReader::new(&data);
        assert!(r.more_rbsp_data());
        r.skip(2).expect("two bits");
        assert!(r.more_rbsp_data());
        r.skip(6).expect("the rest of the byte");
        assert!(!r.more_rbsp_data());
    }
}
