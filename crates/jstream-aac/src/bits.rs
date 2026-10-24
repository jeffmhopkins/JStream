//! An MSB-first bit writer: AAC's bitstream order (ISO/IEC 14496-3 §1.5.1: "most
//! significant bit first").

/// Writes bits, most significant first, into a byte buffer it borrows.
pub(crate) struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    /// Bits not yet a whole byte, in the low `fill` bits.
    acc: u32,
    fill: u32,
    /// Bits written so far.
    written: usize,
}

impl<'a> BitWriter<'a> {
    /// A writer that appends to `out`.
    pub(crate) fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            acc: 0,
            fill: 0,
            written: 0,
        }
    }

    /// Writes the low `n` bits of `value`, `n <= 24`.
    pub(crate) fn put(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 24, "at most 24 bits at a time");
        debug_assert!(n == 0 || value >> n == 0, "value has more than {n} bits");
        if n == 0 {
            return;
        }
        self.acc = (self.acc << n) | (value & ((1 << n) - 1));
        self.fill += n;
        self.written += n as usize;
        while self.fill >= 8 {
            self.fill -= 8;
            // Truncation keeps the next whole byte.
            self.out.push((self.acc >> self.fill) as u8);
        }
        self.acc &= (1 << self.fill) - 1;
    }

    /// Writes one bit.
    pub(crate) fn bit(&mut self, on: bool) {
        self.put(u32::from(on), 1);
    }

    /// Bits written so far.
    #[cfg(test)]
    pub(crate) fn bits(&self) -> usize {
        self.written
    }

    /// Pads with zero bits to a byte boundary (`byte_alignment()`), and returns the bits
    /// written in all.
    pub(crate) fn finish(mut self) -> usize {
        let pad = (8 - self.fill % 8) % 8;
        self.put(0, pad);
        self.written
    }
}

#[cfg(test)]
mod tests {
    use super::BitWriter;

    #[test]
    fn writes_msb_first_and_aligns() {
        let mut out = Vec::new();
        let mut w = BitWriter::new(&mut out);
        w.put(0b101, 3);
        w.bit(true);
        w.put(0xABCDE, 20);
        w.put(0b1, 1);
        assert_eq!(w.bits(), 25);
        assert_eq!(w.finish(), 32);
        // 101 1 1010 1011 1100 1101 1110 1 0000000
        assert_eq!(out, [0b1011_1010, 0b1011_1100, 0b1101_1110, 0b1000_0000]);
    }

    #[test]
    fn appends_and_counts_bits() {
        let mut out = vec![0xFF];
        let mut w = BitWriter::new(&mut out);
        for i in 0..100u32 {
            w.put(i % 2, 1);
        }
        assert_eq!(w.finish(), 104);
        assert_eq!(out.len(), 14);
        assert_eq!(out[1], 0b0101_0101);
    }
}
