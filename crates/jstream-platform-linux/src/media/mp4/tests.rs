use std::fs::File;
use std::path::{Path, PathBuf};

use super::{Codec, Mp4};

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/testdata")
        .join(name)
}

pub(crate) fn open(name: &str) -> Mp4 {
    Mp4::open(File::open(testdata(name)).expect("opens"))
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
    assert!(Mp4::open(file).expect("reads").is_none());
}
