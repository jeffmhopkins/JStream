//! Noiseless coding (ISO/IEC 13818-7 §9 and Annex B's "noiseless coding" part): the
//! spectrum codebooks' bit counts and codewords, scalefactor differences, and sectioning.
//!
//! Sectioning follows Annex B's description: every scalefactor band starts as its own
//! section with its cheapest codebook, and adjacent sections are merged while a merge
//! saves bits, the largest saving first.

use crate::bits::BitWriter;
use crate::tables::{
    BOOK1_CODE, BOOK1_LEN, BOOK2_CODE, BOOK2_LEN, BOOK3_CODE, BOOK3_LEN, BOOK4_CODE, BOOK4_LEN,
    BOOK5_CODE, BOOK5_LEN, BOOK6_CODE, BOOK6_LEN, BOOK7_CODE, BOOK7_LEN, BOOK8_CODE, BOOK8_LEN,
    BOOK9_CODE, BOOK9_LEN, BOOK10_CODE, BOOK10_LEN, BOOK11_CODE, BOOK11_LEN, SF_CODE, SF_LEN,
};

/// `ZERO_HCB`: the band is all zeros and carries no scalefactor.
pub(crate) const ZERO_HCB: u8 = 0;
/// `ESC_HCB`: pairs up to 15, and 16 as an escape for larger values.
pub(crate) const ESC_HCB: u8 = 11;
/// The largest quantized magnitude the escape codebook can carry (an escape word of 13
/// bits at most).
pub(crate) const MAX_QUANT: i32 = 8191;
/// A cost for a codebook that can't code a band.
pub(crate) const NEVER: u32 = u32::MAX / 64;
/// The number of codebooks a section may name, `ZERO_HCB` through `INTENSITY_HCB` (15).
/// The reserved book 12 and `NOISE_HCB` (13, PNS, never emitted) always cost [`NEVER`];
/// the intensity books cost nothing on the bands they're chosen for and [`NEVER`]
/// elsewhere, so sectioning never merges an intensity band with any other.
pub(crate) const BOOKS: usize = 16;

/// One spectrum codebook (ISO/IEC 13818-7 Table 9.2 lists their parameters).
struct Book {
    /// Values per codeword: 4 or 2.
    dim: usize,
    /// Whether the codeword carries the signs (otherwise one sign bit per non-zero value
    /// follows it).
    signed: bool,
    /// The largest absolute value (`LAV`); 16 for the escape book.
    lav: i32,
    len: &'static [u8],
    code: &'static [u16],
}

static BOOK: [Book; 11] = [
    Book {
        dim: 4,
        signed: true,
        lav: 1,
        len: &BOOK1_LEN,
        code: &BOOK1_CODE,
    },
    Book {
        dim: 4,
        signed: true,
        lav: 1,
        len: &BOOK2_LEN,
        code: &BOOK2_CODE,
    },
    Book {
        dim: 4,
        signed: false,
        lav: 2,
        len: &BOOK3_LEN,
        code: &BOOK3_CODE,
    },
    Book {
        dim: 4,
        signed: false,
        lav: 2,
        len: &BOOK4_LEN,
        code: &BOOK4_CODE,
    },
    Book {
        dim: 2,
        signed: true,
        lav: 4,
        len: &BOOK5_LEN,
        code: &BOOK5_CODE,
    },
    Book {
        dim: 2,
        signed: true,
        lav: 4,
        len: &BOOK6_LEN,
        code: &BOOK6_CODE,
    },
    Book {
        dim: 2,
        signed: false,
        lav: 7,
        len: &BOOK7_LEN,
        code: &BOOK7_CODE,
    },
    Book {
        dim: 2,
        signed: false,
        lav: 7,
        len: &BOOK8_LEN,
        code: &BOOK8_CODE,
    },
    Book {
        dim: 2,
        signed: false,
        lav: 12,
        len: &BOOK9_LEN,
        code: &BOOK9_CODE,
    },
    Book {
        dim: 2,
        signed: false,
        lav: 12,
        len: &BOOK10_LEN,
        code: &BOOK10_CODE,
    },
    Book {
        dim: 2,
        signed: false,
        lav: 16,
        len: &BOOK11_LEN,
        code: &BOOK11_CODE,
    },
];

/// The first codebook whose range covers `max_abs` (the pair of books from it on are
/// the candidates; books with a larger range are too).
pub(crate) fn first_book(max_abs: i32) -> u8 {
    match max_abs {
        0 => ZERO_HCB,
        1 => 1,
        2 => 3,
        3..=4 => 5,
        5..=7 => 7,
        8..=12 => 9,
        _ => ESC_HCB,
    }
}

/// The codebook index of one tuple, and the number of sign bits that follow it.
fn index(book: &Book, values: &[i32]) -> (usize, u32) {
    let mut idx = 0usize;
    let mut signs = 0;
    if book.signed {
        let modulus = 2 * book.lav as usize + 1;
        for &v in values {
            idx = idx * modulus + (v + book.lav) as usize;
        }
    } else {
        let modulus = book.lav as usize + 1;
        for &v in values {
            let a = v.unsigned_abs().min(book.lav as u32) as usize;
            idx = idx * modulus + a;
            signs += u32::from(v != 0);
        }
    }
    (idx, signs)
}

/// The bits of an escape sequence for magnitude `a >= 16`: `N - 4` ones, a zero and `N`
/// bits, `N = floor(log2 a)`.
fn escape_bits(a: u32) -> u32 {
    let n = 31 - a.leading_zeros();
    (n - 4) + 1 + n
}

/// The bits `book` (1 to 11) spends on `q`, which must be within its range.
pub(crate) fn band_bits(book: u8, q: &[i32]) -> u32 {
    let b = &BOOK[usize::from(book) - 1];
    let mut bits = 0;
    for t in q.chunks_exact(b.dim) {
        let (idx, signs) = index(b, t);
        bits += u32::from(b.len[idx]) + signs;
        if book == ESC_HCB {
            for &v in t {
                let a = v.unsigned_abs();
                if a >= 16 {
                    bits += escape_bits(a);
                }
            }
        }
    }
    bits
}

/// Writes `q` with `book` (1 to 11).
pub(crate) fn write_band(book: u8, q: &[i32], w: &mut BitWriter<'_>) {
    let b = &BOOK[usize::from(book) - 1];
    for t in q.chunks_exact(b.dim) {
        let (idx, _) = index(b, t);
        w.put(u32::from(b.code[idx]), u32::from(b.len[idx]));
        if !b.signed {
            for &v in t {
                if v != 0 {
                    w.bit(v < 0);
                }
            }
        }
        if book == ESC_HCB {
            for &v in t {
                let a = v.unsigned_abs();
                if a >= 16 {
                    let n = 31 - a.leading_zeros();
                    // N - 4 ones, then the separating zero.
                    w.put((1 << (n - 4)) - 1, n - 4);
                    w.bit(false);
                    w.put(a - (1 << n), n);
                }
            }
        }
    }
}

/// The cost of a band under every codebook it fits, in `out` ([`NEVER`] for the others).
/// With `all` false, only the two cheapest-range books are tried, which is what the rate
/// loop's estimate uses.
pub(crate) fn band_costs(q: &[i32], all: bool, out: &mut [u32; BOOKS]) {
    let max_abs = q.iter().map(|v| v.abs()).max().unwrap_or(0);
    out.fill(NEVER);
    let first = first_book(max_abs);
    if first == ZERO_HCB {
        out[0] = 0;
        return;
    }
    let last = if all {
        ESC_HCB
    } else {
        (first + 1).min(ESC_HCB)
    };
    for book in first..=last {
        out[usize::from(book)] = band_bits(book, q);
    }
}

/// The bits of a scalefactor difference, `-60..=60`.
pub(crate) fn sf_bits(diff: i32) -> u32 {
    debug_assert!((-60..=60).contains(&diff));
    u32::from(SF_LEN[(diff + 60) as usize])
}

/// Writes a scalefactor difference, `-60..=60`.
pub(crate) fn write_sf(diff: i32, w: &mut BitWriter<'_>) {
    debug_assert!((-60..=60).contains(&diff));
    let i = (diff + 60) as usize;
    w.put(SF_CODE[i], u32::from(SF_LEN[i]));
}

/// One section: bands `start..end` coded with `book`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Section {
    pub(crate) book: u8,
    pub(crate) start: u8,
    pub(crate) end: u8,
}

/// The bits of a section header of `len` bands: the codebook and the length in
/// `len_bits`-bit increments, escaped at `2^len_bits - 1`.
pub(crate) fn header_bits(len: usize, len_bits: u32) -> u32 {
    let esc = (1usize << len_bits) - 1;
    4 + len_bits * (len / esc + 1) as u32
}

/// The cheapest codebook for summed costs, and its cost.
fn cheapest(costs: &[u32; BOOKS]) -> (u8, u32) {
    let mut best = (0u8, NEVER);
    for (book, &c) in costs.iter().enumerate() {
        if c < best.1 {
            best = (book as u8, c);
        }
    }
    best
}

/// An upper bound on the bits of the sections and spectrum: each band its cheapest
/// codebook, and a new section wherever that codebook changes. Greedy merging only
/// lowers it.
pub(crate) fn estimate(costs: &[[u32; BOOKS]], len_bits: u32) -> u32 {
    let mut bits = 0;
    let mut run_book = u8::MAX;
    let mut run_len = 0;
    for c in costs {
        let (book, cost) = cheapest(c);
        bits += cost;
        if book != run_book {
            if run_len > 0 {
                bits += header_bits(run_len, len_bits);
            }
            run_book = book;
            run_len = 0;
        }
        run_len += 1;
    }
    if run_len > 0 {
        bits += header_bits(run_len, len_bits);
    }
    bits
}

/// Greedy sectioning of `costs` (one entry per band of one window group) into `out`.
/// Returns the number of sections and their total bits (headers and spectrum).
pub(crate) fn sections(
    costs: &[[u32; BOOKS]],
    len_bits: u32,
    out: &mut [Section],
    sums: &mut [[u32; BOOKS]],
) -> (usize, u32) {
    let n = costs.len();
    debug_assert!(out.len() >= n && sums.len() >= n);
    // One section per band to start with.
    for (i, c) in costs.iter().enumerate() {
        sums[i] = *c;
        out[i] = Section {
            book: cheapest(c).0,
            start: i as u8,
            end: i as u8 + 1,
        };
    }
    let mut count = n;
    let total = |s: &Section, sum: &[u32; BOOKS]| {
        cheapest(sum).1 + header_bits(usize::from(s.end - s.start), len_bits)
    };
    loop {
        let mut best: Option<(usize, u32)> = None;
        for i in 0..count.saturating_sub(1) {
            let mut merged = [0u32; BOOKS];
            for (k, m) in merged.iter_mut().enumerate() {
                *m = (sums[i][k] + sums[i + 1][k]).min(NEVER);
            }
            let joined = Section {
                book: 0,
                start: out[i].start,
                end: out[i + 1].end,
            };
            let apart = total(&out[i], &sums[i]) + total(&out[i + 1], &sums[i + 1]);
            let together = total(&joined, &merged);
            if together < apart && best.is_none_or(|(_, s)| apart - together > s) {
                best = Some((i, apart - together));
            }
        }
        let Some((i, _)) = best else { break };
        let next = sums[i + 1];
        for (sum, add) in sums[i].iter_mut().zip(next) {
            *sum = (*sum + add).min(NEVER);
        }
        out[i].end = out[i + 1].end;
        for j in i + 1..count - 1 {
            out[j] = out[j + 1];
            sums[j] = sums[j + 1];
        }
        count -= 1;
    }
    let mut bits = 0;
    for i in 0..count {
        out[i].book = cheapest(&sums[i]).0;
        bits += total(&out[i], &sums[i]);
    }
    (count, bits)
}

/// Writes one window group's section data.
pub(crate) fn write_sections(sections: &[Section], len_bits: u32, w: &mut BitWriter<'_>) {
    let esc = (1u32 << len_bits) - 1;
    for s in sections {
        w.put(u32::from(s.book), 4);
        let mut len = u32::from(s.end - s.start);
        while len >= esc {
            w.put(esc, len_bits);
            len -= esc;
        }
        w.put(len, len_bits);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BOOK, BOOKS, ESC_HCB, NEVER, Section, band_bits, band_costs, estimate, first_book,
        header_bits, index, sections, sf_bits, write_band, write_sections,
    };
    use crate::bits::BitWriter;
    use crate::tables::{SF_CODE, SF_LEN};

    /// Every codebook is a complete prefix code: no codeword is a prefix of another, and
    /// the Kraft sum is exactly one.
    #[test]
    fn codebooks_are_complete_prefix_codes() {
        let check = |lens: &[u8], codes: &[u32]| {
            let mut words: Vec<String> = lens
                .iter()
                .zip(codes)
                .map(|(&l, &c)| {
                    assert!(c >> l == 0, "code {c:#x} longer than {l}");
                    format!("{c:0width$b}", width = usize::from(l))
                })
                .collect();
            words.sort();
            for pair in words.windows(2) {
                assert!(
                    !pair[1].starts_with(&pair[0]),
                    "{} prefixes {}",
                    pair[0],
                    pair[1]
                );
            }
            let kraft: f64 = lens.iter().map(|&l| 0.5f64.powi(i32::from(l))).sum();
            assert!((kraft - 1.0).abs() < 1e-12, "kraft {kraft}");
        };
        for b in &BOOK {
            let codes: Vec<u32> = b.code.iter().map(|&c| u32::from(c)).collect();
            let entries = if b.signed {
                (2 * b.lav as usize + 1).pow(b.dim as u32)
            } else {
                (b.lav as usize + 1).pow(b.dim as u32)
            };
            assert_eq!(b.len.len(), entries);
            check(b.len, &codes);
        }
        assert_eq!(SF_LEN.len(), 121);
        check(&SF_LEN, &SF_CODE);
        // The zero difference is the one-bit codeword "0".
        assert_eq!((SF_LEN[60], SF_CODE[60]), (1, 0));
        assert_eq!(sf_bits(0), 1);
    }

    #[test]
    fn indices_follow_the_standard_layout() {
        // Signed quads: (w+1)*27 + (x+1)*9 + (y+1)*3 + (z+1); all zeros is the centre.
        assert_eq!(index(&BOOK[0], &[0, 0, 0, 0]), (40, 0));
        assert_eq!(index(&BOOK[0], &[-1, -1, -1, -1]), (0, 0));
        assert_eq!(index(&BOOK[0], &[1, 1, 1, 1]), (80, 0));
        // Unsigned quads carry sign bits.
        assert_eq!(index(&BOOK[2], &[0, -2, 1, 0]), (2 * 9 + 3, 2));
        // Signed pairs centre on (0, 0) = 40.
        assert_eq!(index(&BOOK[4], &[0, 0]), (40, 0));
        // The escape book clamps to 16.
        assert_eq!(index(&BOOK[10], &[100, -3]), (16 * 17 + 3, 2));
        // Books 1 and 2: the all-zero quad is the shortest codeword.
        assert_eq!(BOOK[0].len[40], 1);
    }

    #[test]
    fn first_books_cover_their_range() {
        for v in 0..=20 {
            let b = first_book(v);
            if b > 0 {
                assert!(BOOK[usize::from(b) - 1].lav >= v.min(16));
            }
        }
        assert_eq!(first_book(0), 0);
        assert_eq!(first_book(8191), ESC_HCB);
    }

    #[test]
    fn written_bits_match_counted_bits() {
        let q = [
            0, 1, -1, 0, 2, -2, 0, 0, 3, -4, 4, 0, 7, -7, 5, 0, 12, -12, 9, 1, 16, -17, 100, -8191,
        ];
        for book in 1..=11u8 {
            let lav = BOOK[usize::from(book) - 1].lav;
            let fits: Vec<i32> = if book == ESC_HCB {
                q.to_vec()
            } else {
                q.iter().map(|&v| v.clamp(-lav, lav)).collect()
            };
            let mut out = Vec::new();
            let mut w = BitWriter::new(&mut out);
            write_band(book, &fits, &mut w);
            assert_eq!(w.bits() as u32, band_bits(book, &fits), "book {book}");
        }
    }

    #[test]
    fn escape_lengths() {
        // 16: N = 4, so no ones, the zero and 4 bits.
        let mut out = Vec::new();
        let mut w = BitWriter::new(&mut out);
        write_band(ESC_HCB, &[16, 0], &mut w);
        let code = u32::from(BOOK[10].len[16 * 17]);
        assert_eq!(w.bits() as u32, code + 1 + 1 + 4);
        assert_eq!(
            band_bits(ESC_HCB, &[8191, 0]),
            u32::from(BOOK[10].len[16 * 17]) + 1 + 21
        );
    }

    #[test]
    fn sectioning_merges_when_it_saves() {
        let bands: Vec<Vec<i32>> = vec![
            vec![0; 4],
            vec![0; 4],
            vec![1, 0, 0, 0],
            vec![0; 4],
            vec![1, -1, 0, 1],
            vec![5, 0, 0, 0],
            vec![0; 4],
        ];
        let mut costs = vec![[NEVER; BOOKS]; bands.len()];
        for (b, c) in bands.iter().zip(costs.iter_mut()) {
            band_costs(b, true, c);
        }
        let mut out = [Section::default(); 16];
        let mut sums = [[0; BOOKS]; 16];
        let (n, bits) = sections(&costs, 5, &mut out, &mut sums);
        assert!(n < bands.len());
        assert!(bits <= estimate(&costs, 5));
        // The sections tile the bands in order, and each book fits its bands.
        assert_eq!(out[0].start, 0);
        assert_eq!(out[n - 1].end as usize, bands.len());
        for s in &out[..n] {
            for b in s.start..s.end {
                assert!(costs[usize::from(b)][usize::from(s.book)] < NEVER);
            }
        }
        for pair in out[..n].windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        // Writing them costs what was counted for the headers.
        let mut buf = Vec::new();
        let mut w = BitWriter::new(&mut buf);
        write_sections(&out[..n], 5, &mut w);
        let headers: u32 = out[..n]
            .iter()
            .map(|s| header_bits(usize::from(s.end - s.start), 5))
            .sum();
        assert_eq!(w.bits() as u32, headers);
    }

    #[test]
    fn long_sections_escape_their_length() {
        assert_eq!(header_bits(30, 5), 9);
        assert_eq!(header_bits(31, 5), 14);
        assert_eq!(header_bits(7, 3), 10);
        let mut buf = Vec::new();
        let mut w = BitWriter::new(&mut buf);
        write_sections(
            &[Section {
                book: 0,
                start: 0,
                end: 31,
            }],
            5,
            &mut w,
        );
        assert_eq!(w.bits(), 14);
    }
}
