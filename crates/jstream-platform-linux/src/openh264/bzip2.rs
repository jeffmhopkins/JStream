//! A bzip2 decompressor in safe Rust, for the one file JStream unpacks: Cisco's
//! `libopenh264-*.so.bz2` (ADR-0061).
//!
//! The `bzip2` crate's pure-Rust backend (`libbz2-rs-sys`) is licensed `bzip2-1.0.6`,
//! which `deny.toml` allows only at build time, for CEF's download. Linking it into the
//! engine would widen a licence exception, which is the maintainer's call (ADR-0043); a
//! decoder is a few hundred lines. Only decompression is here. The input is checked
//! against the pinned SHA-256 before it reaches this decoder, but the decoder still
//! treats it as untrusted: every length is bounded, the output is capped, and each
//! block's and the stream's CRC must match.

use std::fmt;

/// Why a bzip2 stream didn't decompress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bzip2Error(&'static str);

impl fmt::Display for Bzip2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad bzip2 data: {}", self.0)
    }
}

impl std::error::Error for Bzip2Error {}

type Result<T> = std::result::Result<T, Bzip2Error>;

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;
const MAX_GROUPS: usize = 6;
const MIN_GROUPS: usize = 2;
const GROUP_SIZE: usize = 50;
const MAX_CODE_LEN: u32 = 20;
/// bzip2 1.0.8 accepts more selectors than this and ignores the extra ones.
const MAX_SELECTORS: usize = 18_002;

/// Decompresses `input`, one bzip2 stream or several concatenated, refusing to produce
/// more than `max_output` bytes.
///
/// # Errors
/// [`Bzip2Error`] when the data is malformed, a CRC doesn't match, or the output would
/// exceed `max_output`.
pub fn decompress(input: &[u8], max_output: usize) -> Result<Vec<u8>> {
    let mut bits = Bits::new(input);
    let mut out = Vec::new();
    loop {
        stream(&mut bits, &mut out, max_output)?;
        bits.align();
        if bits.at_end() {
            return Ok(out);
        }
    }
}

/// One stream: the `BZh` header, its blocks and the end-of-stream marker.
fn stream(bits: &mut Bits<'_>, out: &mut Vec<u8>, max_output: usize) -> Result<()> {
    if bits.take(24)? != u64::from(u32::from_be_bytes([0, b'B', b'Z', b'h'])) {
        return Err(Bzip2Error("missing the BZh header"));
    }
    let level = bits.take(8)?;
    if !(u64::from(b'1')..=u64::from(b'9')).contains(&level) {
        return Err(Bzip2Error("block size out of range"));
    }
    let max_block = usize::try_from(level - u64::from(b'0')).unwrap_or(9) * 100_000;
    let mut combined: u32 = 0;
    loop {
        match bits.take(48)? {
            BLOCK_MAGIC => {
                let expected = u32::try_from(bits.take(32)?).unwrap_or(0);
                let start = out.len();
                block(bits, max_block, out, max_output)?;
                if crc32(&out[start..]) != expected {
                    return Err(Bzip2Error("block CRC mismatch"));
                }
                combined = combined.rotate_left(1) ^ expected;
            }
            END_MAGIC => {
                let expected = u32::try_from(bits.take(32)?).unwrap_or(0);
                return if expected == combined {
                    Ok(())
                } else {
                    Err(Bzip2Error("stream CRC mismatch"))
                };
            }
            _ => return Err(Bzip2Error("bad block marker")),
        }
    }
}

/// One block, decoded and appended to `out`.
fn block(
    bits: &mut Bits<'_>,
    max_block: usize,
    out: &mut Vec<u8>,
    max_output: usize,
) -> Result<()> {
    if bits.take(1)? != 0 {
        return Err(Bzip2Error("randomised blocks are not supported"));
    }
    let orig_ptr = usize::try_from(bits.take(24)?).unwrap_or(usize::MAX);

    // The symbols in use: 16 ranges of 16 bytes each.
    let ranges = bits.take(16)?;
    let mut in_use = Vec::with_capacity(256);
    for range in 0..16u8 {
        if ranges & (1 << (15 - range)) != 0 {
            let present = bits.take(16)?;
            for low in 0..16u8 {
                if present & (1 << (15 - low)) != 0 {
                    in_use.push(range * 16 + low);
                }
            }
        }
    }
    if in_use.is_empty() {
        return Err(Bzip2Error("no symbols in use"));
    }
    let alpha_size = in_use.len() + 2;

    let groups = usize::try_from(bits.take(3)?).unwrap_or(0);
    if !(MIN_GROUPS..=MAX_GROUPS).contains(&groups) {
        return Err(Bzip2Error("table count out of range"));
    }
    let selector_count = usize::try_from(bits.take(15)?).unwrap_or(0);
    if selector_count == 0 {
        return Err(Bzip2Error("no selectors"));
    }
    let mut mtf_groups: Vec<u8> = (0..groups as u8).collect();
    let mut selectors = Vec::with_capacity(selector_count.min(MAX_SELECTORS));
    for _ in 0..selector_count {
        let mut j = 0usize;
        while bits.take(1)? == 1 {
            j += 1;
            if j >= groups {
                return Err(Bzip2Error("selector out of range"));
            }
        }
        let g = mtf_groups.remove(j);
        mtf_groups.insert(0, g);
        if selectors.len() < MAX_SELECTORS {
            selectors.push(g);
        }
    }

    let mut tables = Vec::with_capacity(groups);
    for _ in 0..groups {
        let mut lengths = Vec::with_capacity(alpha_size);
        let mut len = u32::try_from(bits.take(5)?).unwrap_or(0);
        for _ in 0..alpha_size {
            loop {
                if !(1..=MAX_CODE_LEN).contains(&len) {
                    return Err(Bzip2Error("code length out of range"));
                }
                if bits.take(1)? == 0 {
                    break;
                }
                if bits.take(1)? == 0 {
                    len += 1;
                } else {
                    len -= 1;
                }
            }
            lengths.push(len);
        }
        tables.push(Huffman::new(&lengths)?);
    }

    // Huffman, run-length (RUNA/RUNB) and move-to-front decoding into `tt`'s source.
    let eob = alpha_size - 1;
    let mut mtf: Vec<u16> = (0..in_use.len() as u16).collect();
    let mut data: Vec<u8> = Vec::with_capacity(max_block);
    let mut counts = [0usize; 256];
    let mut run = 0usize;
    let mut run_weight = 1usize;
    let mut decoded = 0usize;
    loop {
        let group = decoded / GROUP_SIZE;
        let table = selectors
            .get(group)
            .and_then(|&s| tables.get(usize::from(s)))
            .ok_or(Bzip2Error("ran out of selectors"))?;
        let sym = table.decode(bits)?;
        decoded += 1;
        if sym <= 1 {
            // RUNA adds the weight, RUNB twice it: bijective base 2.
            run = run
                .checked_add(run_weight << sym)
                .filter(|&r| r <= max_block)
                .ok_or(Bzip2Error("run too long"))?;
            run_weight = run_weight
                .checked_mul(2)
                .ok_or(Bzip2Error("run too long"))?;
            continue;
        }
        if run > 0 {
            if data.len() + run > max_block {
                return Err(Bzip2Error("block too large"));
            }
            let byte = in_use[usize::from(mtf[0])];
            data.resize(data.len() + run, byte);
            counts[usize::from(byte)] += run;
            run = 0;
            run_weight = 1;
        }
        if sym == eob {
            break;
        }
        let index = sym - 1;
        if index >= mtf.len() {
            return Err(Bzip2Error("move-to-front index out of range"));
        }
        let m = mtf.remove(index);
        mtf.insert(0, m);
        if data.len() >= max_block {
            return Err(Bzip2Error("block too large"));
        }
        let byte = in_use[usize::from(m)];
        data.push(byte);
        counts[usize::from(byte)] += 1;
    }
    if orig_ptr >= data.len() {
        return Err(Bzip2Error("origin pointer out of range"));
    }

    // Inverse Burrows-Wheeler transform.
    let mut next = vec![0u32; data.len()];
    let mut start = [0usize; 256];
    let mut sum = 0;
    for (s, c) in start.iter_mut().zip(counts) {
        *s = sum;
        sum += c;
    }
    for (i, &b) in data.iter().enumerate() {
        let slot = &mut start[usize::from(b)];
        next[*slot] = u32::try_from(i).map_err(|_| Bzip2Error("block too large"))?;
        *slot += 1;
    }

    // Undo the initial run-length step: four equal bytes are followed by a count of
    // further copies.
    let mut pos = next[orig_ptr] as usize;
    let mut last: Option<u8> = None;
    let mut same = 0;
    for _ in 0..data.len() {
        let byte = data[pos];
        pos = next[pos] as usize;
        if same == 4 {
            let extra = usize::from(byte);
            if out.len() + extra > max_output {
                return Err(Bzip2Error("output larger than allowed"));
            }
            let repeat = last.unwrap_or(0);
            out.resize(out.len() + extra, repeat);
            same = 0;
            last = None;
            continue;
        }
        if out.len() >= max_output {
            return Err(Bzip2Error("output larger than allowed"));
        }
        out.push(byte);
        if last == Some(byte) {
            same += 1;
        } else {
            last = Some(byte);
            same = 1;
        }
    }
    Ok(())
}

/// A canonical Huffman table, decoded one bit at a time.
struct Huffman {
    /// The first code of each length.
    first: [u32; MAX_CODE_LEN as usize + 1],
    /// How many codes each length has.
    count: [u32; MAX_CODE_LEN as usize + 1],
    /// Where each length's symbols start in `symbols`.
    offset: [u32; MAX_CODE_LEN as usize + 1],
    /// The symbols, ordered by code length, then value.
    symbols: Vec<usize>,
}

impl Huffman {
    fn new(lengths: &[u32]) -> Result<Self> {
        let mut count = [0u32; MAX_CODE_LEN as usize + 1];
        for &len in lengths {
            count[len as usize] += 1;
        }
        let mut first = [0u32; MAX_CODE_LEN as usize + 1];
        let mut offset = [0u32; MAX_CODE_LEN as usize + 1];
        let mut code = 0u32;
        let mut index = 0u32;
        for len in 1..=MAX_CODE_LEN as usize {
            first[len] = code;
            offset[len] = index;
            code += count[len];
            index += count[len];
            if code > 1 << len {
                return Err(Bzip2Error("over-subscribed code lengths"));
            }
            code <<= 1;
        }
        let mut symbols: Vec<usize> = (0..lengths.len()).collect();
        symbols.sort_by_key(|&s| (lengths[s], s));
        Ok(Self {
            first,
            count,
            offset,
            symbols,
        })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<usize> {
        let mut code = 0u32;
        for len in 1..=MAX_CODE_LEN as usize {
            code = (code << 1) | u32::try_from(bits.take(1)?).unwrap_or(0);
            let delta = code.wrapping_sub(self.first[len]);
            if delta < self.count[len] {
                return self
                    .symbols
                    .get((self.offset[len] + delta) as usize)
                    .copied()
                    .ok_or(Bzip2Error("bad Huffman code"));
            }
        }
        Err(Bzip2Error("bad Huffman code"))
    }
}

/// Reads bits most significant first.
struct Bits<'a> {
    data: &'a [u8],
    /// The next bit to read, counted from the start.
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// The next `n` bits (at most 56) as a number.
    fn take(&mut self, n: u32) -> Result<u64> {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self
                .data
                .get(self.pos / 8)
                .ok_or(Bzip2Error("unexpected end of data"))?;
            let bit = (byte >> (7 - self.pos % 8)) & 1;
            v = (v << 1) | u64::from(bit);
            self.pos += 1;
        }
        Ok(v)
    }

    /// Skips to the next byte boundary, where a concatenated stream would start.
    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    fn at_end(&self) -> bool {
        self.pos / 8 >= self.data.len()
    }
}

/// bzip2's CRC-32: polynomial `0x04C11DB7`, most significant bit first, initial and final
/// value inverted.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests;
