use std::fs::File;
use std::path::{Path, PathBuf};

use super::super::demux::Demuxed;
use super::Codec;

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/testdata")
        .join(name)
}

pub(crate) fn open(name: &str) -> Demuxed {
    super::open(File::open(testdata(name)).expect("opens"))
        .expect("parses")
        .expect("is an MP4 file")
}

#[test]
fn reads_an_h264_and_mp3_mp4() {
    let mp4 = open("h264-high.mp4");
    let video = mp4.video.as_ref().expect("video");
    assert!(matches!(video.codec, Codec::Avc(_)), "{:?}", video.codec);
    assert_eq!(video.size, (120, 90));
    assert_eq!(video.samples.len(), 30);
    // The edit list moves the first shown frame to 0, and every frame has its own time.
    let mut pts: Vec<i64> = video.samples.iter().map(|s| s.pts).collect();
    pts.sort_unstable();
    assert_eq!(pts[0], 0);
    let step = i64::from(video.timescale) / 30;
    for (i, p) in pts.iter().enumerate() {
        assert_eq!(*p, i as i64 * step, "{pts:?}");
    }
    // Decode order differs from presentation order (B-frames), and keyframes are every
    // 15 frames.
    assert_ne!(video.samples.iter().map(|s| s.pts).collect::<Vec<_>>(), pts);
    let sync: Vec<usize> = video
        .samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.sync)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(sync, vec![0, 15]);
    assert_eq!(video.duration(), 30 * step);

    let audio = mp4.audio.as_ref().expect("audio");
    assert_eq!(audio.codec, Codec::Mp3);
    assert_eq!(audio.channels, 1);
    assert_eq!(audio.sample_rate, 44_100);
    assert!(!audio.samples.is_empty());
}

#[test]
fn seeks_to_the_sync_sample_before() {
    let mp4 = open("h264-high.mp4");
    let video = mp4.video.as_ref().expect("video");
    let step = i64::from(video.timescale) / 30;
    assert_eq!(video.seek_index(0), 0);
    assert_eq!(video.seek_index(14 * step), 0);
    assert_eq!(video.seek_index(15 * step), 15);
    assert_eq!(video.seek_index(29 * step), 15);
    assert_eq!(video.seek_index(1_000 * step), 15);
}

#[test]
fn reads_a_quicktime_movie() {
    let mov = open("h264-baseline.mov");
    let video = mov.video.as_ref().expect("video");
    assert_eq!(video.size, (48, 32));
    assert_eq!(video.samples.len(), 15);
    assert!(mov.audio.is_none());
    // No B-frames: decode order is presentation order.
    assert!(video.samples.windows(2).all(|w| w[0].pts < w[1].pts));
}

#[test]
fn other_files_are_not_mp4() {
    let file = File::open(testdata("tone.flac")).expect("opens");
    assert!(super::open(file).expect("reads").is_none());
}

#[test]
fn a_box_claiming_the_largest_size_doesnt_overflow() {
    // `ftyp` with a 64-bit size of u64::MAX: refused or ignored, never a panic.
    let mut bytes = vec![0, 0, 0, 1];
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(&u64::MAX.to_be_bytes());
    let path = std::env::temp_dir().join(format!("jstream-mp4-test-{}.mp4", std::process::id()));
    std::fs::write(&path, &bytes).expect("writes");
    let file = File::open(&path).expect("opens");
    let _ = std::fs::remove_file(&path);
    assert!(super::open(file).is_err());
}

#[test]
fn a_track_lists_no_more_samples_than_the_file_can_hold() {
    // `stsz` with one size for every sample: version and flags, the size, the count.
    let stsz = |size: u32, count: u32| {
        let mut b = vec![0; 4];
        b.extend_from_slice(&size.to_be_bytes());
        b.extend_from_slice(&count.to_be_bytes());
        b
    };
    let table = |stsz: &[u8]| {
        // One chunk at 0 holding every sample, and one time step.
        let stco = [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        let stsc = [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 10, 0, 0, 0, 1];
        let stts = [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 10, 0, 0, 0, 1];
        super::sample_table(
            &[
                (*b"stsz", stsz),
                (*b"stco", &stco),
                (*b"stsc", &stsc),
                (*b"stts", &stts),
            ],
            0,
            1_000,
        )
    };
    assert_eq!(table(&stsz(10, 10)).expect("fits").len(), 10);
    // A million one-byte samples in a thousand-byte file, or ten of a kilobyte each.
    assert!(table(&stsz(1, 1_000_000)).is_err());
    assert!(table(&stsz(1_000, 10)).is_err());
}

#[test]
fn reads_an_av1_mp4s_configuration_obus() {
    let mp4 = open("av1-aom.mp4");
    let video = mp4.video.as_ref().expect("video");
    let Codec::Av1(obus) = &video.codec else {
        panic!("{:?}", video.codec);
    };
    assert_eq!(video.size, (208, 120));
    let mut parser = crate::media::av1::Parser::new();
    assert!(parser.parse(obus).expect("parses").is_empty());
    let seq = parser.sequence().expect("av1C holds the sequence header");
    assert_eq!((seq.max_frame_width, seq.max_frame_height), (208, 120));
}
