use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::demux::{Codec, Demuxed};

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/testdata")
        .join(name)
}

fn open(name: &str) -> Demuxed {
    super::open(File::open(testdata(name)).expect("opens"))
        .expect("parses")
        .expect("is a Matroska file")
}

/// Opens `bytes` written to a temporary file.
fn open_bytes(bytes: &[u8]) -> Option<Demuxed> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "jstream-mkv-test-{}-{}.mkv",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    File::create(&path)
        .and_then(|mut f| f.write_all(bytes))
        .expect("writes");
    let file = File::open(&path).expect("opens");
    let _ = std::fs::remove_file(&path);
    super::open(file).expect("parses")
}

#[test]
fn reads_the_same_h264_frames_as_the_mp4_it_was_remuxed_from() {
    let mut mkv = open("h264-vorbis.mkv");
    let mut mp4 = super::super::mp4::tests::open("h264-high.mp4");
    let video = mkv.video.clone().expect("video");
    let reference = mp4.video.clone().expect("video");
    assert!(matches!(video.codec, Codec::Avc(_)), "{:?}", video.codec);
    assert_eq!(video.codec, reference.codec, "the same avcC record");
    assert_eq!(video.size, (120, 90));
    assert_eq!(video.timescale, 1000);
    assert_eq!(video.samples.len(), reference.samples.len());
    let keys: Vec<usize> = (0..video.samples.len())
        .filter(|i| video.samples[*i].sync)
        .collect();
    assert_eq!(keys, [0, 15]);
    // ffmpeg starts the remux 3 ms in; the gaps are what must match.
    let start = video.samples[0].pts;
    for (a, b) in video.samples.iter().zip(&reference.samples) {
        assert_eq!(
            mkv.read(&video, a).expect("reads"),
            mp4.read(&reference, b).expect("reads"),
            "the same frame bytes in the same order"
        );
        // Milliseconds against the MP4's own time scale, rounded.
        let ms = b.pts * 1000 / i64::from(reference.timescale);
        assert!((a.pts - start - ms).abs() <= 1, "{} vs {ms}", a.pts - start);
    }
    // Decode times are the presentation times in order.
    assert!(video.samples.windows(2).all(|w| w[0].dts <= w[1].dts));

    let audio = mkv.audio.clone().expect("audio");
    let Codec::Vorbis(headers) = &audio.codec else {
        panic!("{:?}", audio.codec);
    };
    assert_eq!(headers.first(), Some(&2), "Xiph-laced headers");
    assert_eq!((audio.channels, audio.sample_rate), (1, 44_100));
    assert!(audio.samples.iter().all(|s| s.sync));
    assert!(audio.samples.windows(2).all(|w| w[0].pts <= w[1].pts));
}

#[test]
fn names_webm_codecs_it_doesnt_decode() {
    let webm = open("vp9-opus.webm");
    let video = webm.video.as_ref().expect("video");
    assert_eq!(video.codec, Codec::Other("V_VP9".into()));
    let audio = webm.audio.as_ref().expect("audio");
    assert_eq!(audio.codec, Codec::Other("Opus".into()));
}

#[test]
fn other_files_are_not_matroska() {
    assert!(
        super::open(File::open(testdata("h264-high.mp4")).expect("opens"))
            .expect("reads")
            .is_none()
    );
    assert!(open_bytes(b"not a media file at all").is_none());
}

/// An element: its ID's bytes, a size, and a body.
fn el(id: u32, body: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = id
        .to_be_bytes()
        .into_iter()
        .skip_while(|b| *b == 0)
        .collect();
    out.push(0x01); // an eight-byte size
    out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
    out.extend_from_slice(body);
    out
}

/// An element with an unknown size.
fn el_unknown(id: u32, body: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = id
        .to_be_bytes()
        .into_iter()
        .skip_while(|b| *b == 0)
        .collect();
    out.push(0xFF);
    out.extend_from_slice(body);
    out
}

fn uint_el(id: u32, v: u64) -> Vec<u8> {
    el(id, &v.to_be_bytes())
}

/// A file with an MP3 audio track (number 1) whose blocks are `blocks`, in one cluster
/// at `cluster_time`, with `encodings` in the track entry.
fn file(blocks: &[Vec<u8>], encodings: &[u8], unknown_cluster: bool) -> Vec<u8> {
    let mut entry = uint_el(super::id::TRACK_NUMBER, 1);
    entry.extend(uint_el(super::id::TRACK_TYPE, 2));
    entry.extend(el(super::id::CODEC_ID, b"A_MPEG/L3"));
    let mut audio = el(super::id::SAMPLING_FREQUENCY, &44_100.0_f64.to_be_bytes());
    audio.extend(uint_el(super::id::CHANNELS, 2));
    entry.extend(el(super::id::AUDIO, &audio));
    entry.extend_from_slice(encodings);
    let tracks = el(super::id::TRACKS, &el(super::id::TRACK_ENTRY, &entry));
    let mut cluster = uint_el(super::id::TIMESTAMP, 1000);
    for b in blocks {
        cluster.extend(el(super::id::SIMPLE_BLOCK, b));
    }
    let mut segment = el(
        super::id::INFO,
        &uint_el(super::id::TIMESTAMP_SCALE, 1_000_000),
    );
    segment.extend(tracks);
    segment.extend(if unknown_cluster {
        el_unknown(super::id::CLUSTER, &cluster)
    } else {
        el(super::id::CLUSTER, &cluster)
    });
    let mut out = el(super::id::EBML, &el(super::id::DOC_TYPE, b"matroska"));
    out.extend(el_unknown(super::id::SEGMENT, &segment));
    out
}

/// A `SimpleBlock` for track 1 at `rel` with `flags`, a lacing header and frames.
fn simple_block(rel: i16, flags: u8, lacing: &[u8], frames: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![0x81];
    out.extend_from_slice(&rel.to_be_bytes());
    out.push(flags);
    out.extend_from_slice(lacing);
    for f in frames {
        out.extend_from_slice(f);
    }
    out
}

/// Each sample's bytes.
fn frames(demuxed: &mut Demuxed) -> Vec<Vec<u8>> {
    let track = demuxed.audio.clone().expect("audio");
    track
        .samples
        .iter()
        .map(|s| demuxed.read(&track, s).expect("reads"))
        .collect()
}

#[test]
fn splits_each_kind_of_lacing() {
    let a: &[u8] = &[1; 300];
    let b: &[u8] = &[2; 5];
    let c: &[u8] = &[3; 7];
    // Xiph: 300 = 255 + 45, then 5; the last size is what is left.
    let xiph = simple_block(0, 0x82, &[2, 255, 45, 5], &[a, b, c]);
    // EBML: 300 as a two-byte vint, then 5 - 300 = -295 (bias 8191), then the rest.
    let first = 0x4000 | 300_u16;
    let delta = u16::try_from(8191 - 295).expect("fits") | 0x4000;
    let mut ebml_lacing = vec![2];
    ebml_lacing.extend_from_slice(&first.to_be_bytes());
    ebml_lacing.extend_from_slice(&delta.to_be_bytes());
    let ebml = simple_block(20, 0x86, &ebml_lacing, &[a, b, c]);
    // Fixed: three frames of five bytes.
    let fixed = simple_block(40, 0x84, &[2], &[b, b, b]);
    let none = simple_block(60, 0x80, &[], &[c]);
    let bytes = file(&[xiph, ebml, fixed, none], &[], false);
    let mut demuxed = open_bytes(&bytes).expect("a Matroska file");
    assert_eq!(
        frames(&mut demuxed),
        [a, b, c, a, b, c, b, b, b, c].map(<[u8]>::to_vec)
    );
    let track = demuxed.audio.expect("audio");
    assert_eq!(track.timescale, 1000);
    // Laced frames are spread between their block and the next.
    let pts: Vec<i64> = track.samples.iter().map(|s| s.pts).collect();
    assert_eq!(
        pts,
        [1000, 1006, 1013, 1020, 1026, 1033, 1040, 1046, 1053, 1060]
    );
}

#[test]
fn puts_stripped_headers_back() {
    let mut comp = uint_el(super::id::CONTENT_COMP_ALGO, 3);
    comp.extend(el(super::id::CONTENT_COMP_SETTINGS, &[0xFF, 0xFB]));
    let encodings = el(
        super::id::CONTENT_ENCODINGS,
        &el(
            super::id::CONTENT_ENCODING,
            &el(super::id::CONTENT_COMPRESSION, &comp),
        ),
    );
    let block = simple_block(0, 0x80, &[], &[&[9, 9]]);
    let mut demuxed = open_bytes(&file(&[block], &encodings, false)).expect("Matroska");
    assert_eq!(frames(&mut demuxed), [vec![0xFF, 0xFB, 9, 9]]);

    // zlib is refused by name.
    let encodings = el(
        super::id::CONTENT_ENCODINGS,
        &el(
            super::id::CONTENT_ENCODING,
            &el(
                super::id::CONTENT_COMPRESSION,
                &uint_el(super::id::CONTENT_COMP_ALGO, 0),
            ),
        ),
    );
    let block = simple_block(0, 0x80, &[], &[&[9, 9]]);
    let demuxed = open_bytes(&file(&[block], &encodings, false)).expect("Matroska");
    assert_eq!(
        demuxed.audio.expect("audio").codec,
        Codec::Other("A_MPEG/L3 compressed (algorithm 0)".into())
    );
}

#[test]
fn reads_a_live_recording_cut_short() {
    let blocks: Vec<Vec<u8>> = (0..4)
        .map(|i| simple_block(i * 26, 0x80, &[], &[&[u8::try_from(i).expect("small"); 10]]))
        .collect();
    let bytes = file(&blocks, &[], true);
    let mut demuxed = open_bytes(&bytes).expect("Matroska");
    assert_eq!(frames(&mut demuxed).len(), 4);
    // Cut in the middle of the last block: the three before it remain.
    let cut = &bytes[..bytes.len() - 4];
    let mut demuxed = open_bytes(cut).expect("Matroska");
    assert_eq!(frames(&mut demuxed).len(), 3);
}

#[test]
fn its_vorbis_audio_decodes() {
    use jstream_platform::media::Timebase;
    use symphonia::core::packet::Packet;
    use symphonia::core::units::{Duration, Timestamp};

    let mut mkv = open("h264-vorbis.mkv");
    let track = mkv.audio.clone().expect("audio");
    let params = track.audio_params().expect("Vorbis decodes");
    let timebase = Timebase::per_second(track.timescale).expect("timebase");
    let mut stream = super::super::audio::AudioStream::new(&params, timebase).expect("opens");
    let mut frames = 0;
    for sample in &track.samples {
        let data = mkv.read(&track, sample).expect("reads");
        let packet = Packet::new(track.id, Timestamp::new(sample.pts), Duration::new(0), data);
        if let Some(block) = stream.decode(&packet).expect("decodes") {
            frames += block.frames();
        }
    }
    // One second at 44.1 kHz, give or take Vorbis's first block.
    assert!(
        (43_000..=44_100 + 2048).contains(&frames),
        "{frames} frames"
    );
}

#[test]
fn a_recording_cut_in_a_block_before_its_cluster_time_keeps_what_came_before() {
    // A block shown before its cluster's time has a negative relative time, so its
    // body starts `81 FF …`. Read as an element, that is an ID with an unknown size.
    let blocks: Vec<Vec<u8>> = [0, 26, -3, 52]
        .into_iter()
        .map(|rel: i16| simple_block(rel, 0x80, &[], &[&[7; 10]]))
        .collect();
    for unknown_cluster in [false, true] {
        let bytes = file(&blocks[..3], &[], unknown_cluster);
        // Cut inside the third block, just past its header.
        let cut = &bytes[..bytes.len() - 8];
        let mut demuxed = open_bytes(cut).expect("Matroska");
        assert_eq!(
            frames(&mut demuxed).len(),
            2,
            "unknown size: {unknown_cluster}"
        );
    }
    // The fixture cut through its Vorbis block at relative time -3.
    let whole = std::fs::read(testdata("h264-vorbis.mkv")).expect("reads");
    for cut in 7_682..=7_707 {
        assert!(open_bytes(&whole[..cut]).is_some(), "cut at {cut}");
    }
}

#[test]
fn an_ebml_lace_of_one_frame_is_one_frame() {
    let blocks = vec![simple_block(0, 0x80 | 0x06, &[0], &[&[9; 10]])];
    let mut demuxed = open_bytes(&file(&blocks, &[], false)).expect("Matroska");
    assert_eq!(frames(&mut demuxed), vec![vec![9; 10]]);
}
