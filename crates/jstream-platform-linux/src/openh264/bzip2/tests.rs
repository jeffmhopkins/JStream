use super::{Bzip2Error, crc32, decompress};

/// The input `testdata/blocks.bz2` was made from (with `bzip2 -1`): words, NUL and 0xFF
/// bytes, and runs of 3 to 682 equal bytes, so it covers the run-length steps and spans
/// several 100 kB blocks.
fn blocks_input() -> Vec<u8> {
    let words: [&[u8]; 6] = [
        b"open",
        b"h264",
        b"cisco",
        b"jstream",
        b"\x00\xff",
        b"frame ",
    ];
    let mut out = Vec::new();
    let mut x: u32 = 1;
    while out.len() < 330_000 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let k = x >> 24;
        if k < 8 {
            let byte = u8::try_from((k * 31) & 0xff).expect("a byte");
            out.extend(std::iter::repeat_n(byte, (k * 97 + 3) as usize));
        } else {
            out.extend_from_slice(words[(k % 6) as usize]);
        }
    }
    out.truncate(330_000);
    out
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn decompresses_what_bzip2_wrote() {
    let blocks = include_bytes!("../testdata/blocks.bz2");
    assert_eq!(
        decompress(blocks, 1 << 20).expect("decompresses"),
        blocks_input()
    );

    let hello = hex(
        "425a6839314159265359cc2e03a10000065180001040040644908020002190320400c2a832d9416e9c\
         079af177245385090cc2e03a10",
    );
    assert_eq!(
        decompress(&hello, 1 << 10).expect("decompresses"),
        b"hello, hello, hello world\n"
    );

    let empty = hex("425a683917724538509000000000");
    assert_eq!(decompress(&empty, 0).expect("decompresses"), b"");

    // Two streams back to back, as `cat a.bz2 b.bz2` makes.
    let both = hex(
        "425a6839314159265359e993fdcd000000010030002000210082b177245385090e993fdcd0\
         425a683931415926535953e72e7000000001000c002000210082b17724538509053e72e700",
    );
    assert_eq!(decompress(&both, 16).expect("decompresses"), b"abcd");
}

#[test]
fn damage_is_refused_not_decoded() {
    let blocks = include_bytes!("../testdata/blocks.bz2");
    // The output cap holds.
    assert_eq!(
        decompress(blocks, 1000),
        Err(Bzip2Error("output larger than allowed"))
    );
    // Truncated anywhere, or with any of a spread of bytes flipped, it is an error and
    // never a panic or a wrong answer.
    for cut in [0, 3, 4, 10, 100, blocks.len() / 2, blocks.len() - 1] {
        assert!(decompress(&blocks[..cut], 1 << 20).is_err(), "cut at {cut}");
    }
    // (The last byte is left alone: its low bits are padding after the stream CRC.)
    for at in (4..blocks.len() - 1).step_by(97) {
        let mut damaged = blocks.to_vec();
        damaged[at] ^= 0x10;
        assert!(decompress(&damaged, 1 << 20).is_err(), "flipped byte {at}");
    }
    // Not bzip2 at all.
    assert!(decompress(b"\x7fELF\x02\x01\x01\0", 1 << 20).is_err());
    // Trailing bytes that aren't another stream.
    let mut trailing = hex("425a683917724538509000000000");
    trailing.extend_from_slice(b"junk");
    assert!(decompress(&trailing, 16).is_err());
}

#[test]
fn crc_matches_bzip2s() {
    // bzip2's CRC-32 is the MSB-first variant (CRC-32/BZIP2); this is its check value.
    assert_eq!(crc32(b"123456789"), 0xFC89_1918);
}
